//! An app process's records, read and written over its own connection to the device
//! ([`tilo_actions::remote::RemoteActionMessage::StoreRequest`]).
//!
//! The daemon started the process and knows it as one app in one workspace
//! ([`crate::seats::AppAt`]): every command on the connection is that app's, there, whatever the
//! frame says. The device's workspace service keeps the records; the process holds no key for
//! them. Each command is decided as the device's one reader decides it for the person the device
//! is, through the app: a change only where they may make it, a record only where they may read
//! it. An agent's call never writes here, and a workspace other than the app's is refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use collab_types::store::wire::{Bytes, Command, MAX_WIRE_BYTES, Reply};
use tilo_actions::remote::{
    ActionCallError, Caller, Pending, ResourceAddress, ResourceReads, StoreCommands, pending,
};
use tilo_broker_core::services::ResourceRequestContext;
use tilo_lock::MutexRecover as _;

use super::services::Daemon;

/// The commands of the process the daemon started as `app` in `workspace`.
pub(crate) struct StoreLane {
    daemon: Weak<Daemon>,
    app: String,
    workspace: String,
    /// The parts of each record's change still to come, by the record they name: a change too
    /// large for one frame goes as several, taken as one write once the last arrives.
    runs: Mutex<HashMap<String, Vec<u8>>>,
}

impl StoreLane {
    pub(crate) fn new(daemon: &Arc<Daemon>, app: &str, workspace: &str) -> Self {
        Self {
            daemon: Arc::downgrade(daemon),
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            runs: Mutex::default(),
        }
    }

    /// `command` as the store takes it, once it is whole: `None` while a change has parts to
    /// come. Refused where the connection may not send it at all.
    fn taken(
        &self,
        caller: &Caller,
        command: serde_json::Value,
    ) -> Result<Option<Command>, ActionCallError> {
        if self.app.is_empty() || self.workspace.is_empty() {
            return Err(ActionCallError::refused(
                "records are read and written by an app the device started in a workspace",
            ));
        }
        if caller.workspace() != Some(self.workspace.as_str()) {
            return Err(ActionCallError::refused(format!(
                "{} was started in another workspace and reads and writes records only there",
                self.app
            )));
        }
        let command: Command =
            serde_json::from_value(command).map_err(|error| ActionCallError::InvalidRequest {
                reason: format!("no store command: {error}"),
            })?;
        if command.writes() && caller.acting().is_agent() {
            return Err(ActionCallError::refused(
                "an agent changes an app's records through the app's actions, never its store",
            ));
        }
        let Command::RecordChange {
            record,
            changes,
            more,
            commit,
        } = command
        else {
            if let Command::RecordChanges { record: None, .. } = command {
                return Err(unnamed_record());
            }
            return Ok(Some(command));
        };
        let record = record.ok_or_else(unnamed_record)?;
        let mut runs = self.runs.lock_recover();
        let gathering = runs.entry(record.clone()).or_default();
        if gathering.len().saturating_add(changes.0.len()) > MAX_WIRE_BYTES.saturating_mul(64) {
            runs.remove(&record);
            return Err(ActionCallError::InvalidRequest {
                reason: "a record's change is larger than the store takes in one write".into(),
            });
        }
        gathering.extend_from_slice(&changes.0);
        if more {
            return Ok(None);
        }
        let changes = runs.remove(&record).unwrap_or_default();
        drop(runs);
        Ok(Some(Command::RecordChange {
            record: Some(record),
            changes: Bytes(changes),
            more: false,
            commit,
        }))
    }
}

/// What a record command naming no record is told: a connection opens no record of its own.
fn unnamed_record() -> ActionCallError {
    ActionCallError::InvalidRequest {
        reason: "a record's changes are read and written by its id".into(),
    }
}

impl StoreCommands for StoreLane {
    fn command(
        &self,
        caller: &Caller,
        command: serde_json::Value,
        kind: Option<String>,
    ) -> Pending<Result<serde_json::Value, ActionCallError>> {
        let command = match self.taken(caller, command) {
            Ok(Some(command)) => command,
            Ok(None) => return Pending::ready(encoded(&Reply::Written)),
            Err(refusal) => return Pending::ready(Err(refusal)),
        };
        let Some(daemon) = self.daemon.upgrade() else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this computer's daemon is stopping".into(),
            }));
        };
        let (app, workspace) = (self.app.clone(), self.workspace.clone());
        let (answer, answered) = pending();
        let task = async move {
            let reply =
                answered_by_store(&daemon, (&app, kind.as_deref()), &workspace, command).await;
            answer.complete(reply.and_then(|reply| encoded(&reply)));
        };
        match tilo_rt::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn(task)),
            Err(_) => drop(tilo_rt::app().spawn(task)),
        }
        answered
    }
}

/// What a connection whose process is no longer the one the daemon started is answered.
pub(crate) struct Refused(pub(crate) ActionCallError);

impl StoreCommands for Refused {
    fn command(
        &self,
        _: &Caller,
        _: serde_json::Value,
        _: Option<String>,
    ) -> Pending<Result<serde_json::Value, ActionCallError>> {
        Pending::ready(Err(self.0.clone()))
    }
}

fn encoded(reply: &Reply) -> Result<serde_json::Value, ActionCallError> {
    serde_json::to_value(reply).map_err(|error| ActionCallError::Unavailable {
        reason: format!("the store's answer cannot be encoded: {error}"),
    })
}

/// `command` from `app`'s process in `workspace`, decided and answered by the device's one store.
async fn answered_by_store(
    daemon: &Daemon,
    (app, kind): (&str, Option<&str>),
    workspace: &str,
    command: Command,
) -> Result<Reply, ActionCallError> {
    let workspaces = daemon.workspaces();
    let cert = workspaces.device_cert();
    // The person this device is, through the app: what its reads and writes are decided as.
    let person =
        Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(workspace);
    let broker = daemon.app_broker();
    let record = match &command {
        Command::RecordChanges { record, .. } | Command::RecordChange { record, .. } => {
            record.clone()
        }
        _ => None,
    };
    let address = ResourceAddress {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: record.clone().unwrap_or_default(),
    };
    let reads = broker
        .as_ref()
        .and_then(tilo_app_host::collab_broker::AppActionBroker::record_reads);
    let through_app = person.clone().through_app(app);
    if command.writes() {
        may_write(reads.as_deref(), &through_app, &address).await?;
    } else if matches!(command, Command::RecordChanges { .. }) {
        may_read(reads.as_deref(), &through_app, &address).await?;
    }
    let mut context =
        ResourceRequestContext::authenticated(app, workspace, person).of_app_process();
    if let Some(record) = &record {
        context = context
            .on_record(record.clone())
            .of_kind(kind.map(str::to_owned));
    }
    if matches!(command, Command::RecordChange { .. }) {
        context = context.refusing_founding(
            broker
                .as_ref()
                .and_then(|broker| broker.founding_refusal(app, kind)),
        );
    }
    let changes = command.writes() && record.is_some();
    let reply = workspaces
        .resource_command(&context, command)
        .await
        .map_err(|reason| ActionCallError::Refused { reason })?;
    // Every window open on the record asks for what it lacks of it.
    if changes
        && matches!(reply, Reply::Written)
        && let (Some(broker), Some(record)) = (&broker, &record)
    {
        broker.record_changed(workspace, app, record);
    }
    Ok(reply)
}

/// Refuse a change the device's one reader does not let `caller` make at `address`.
async fn may_write(
    reads: Option<&dyn ResourceReads>,
    caller: &Caller,
    address: &ResourceAddress,
) -> Result<(), ActionCallError> {
    let decided = match reads {
        Some(reads) => Some(reads.may_write(caller, address).await),
        None => None,
    };
    match decided {
        Some(Ok(())) => Ok(()),
        // No reader answered the write decision: none serves at this device yet, or it cannot
        // decide a change. The record cannot be changed from here now.
        None | Some(Err(ActionCallError::Unavailable { .. })) => {
            Err(ActionCallError::Unavailable {
                reason: "this record can't be changed right now".into(),
            })
        }
        Some(Err(refusal)) => Err(ActionCallError::refused(format!(
            "{} may not change this record: {refusal}",
            address.app
        ))),
    }
}

/// Refuse a read the device's one reader does not let `caller` make at `address`. One it would
/// ask the person about reads, as opening the app asked them already.
async fn may_read(
    reads: Option<&dyn ResourceReads>,
    caller: &Caller,
    address: &ResourceAddress,
) -> Result<(), ActionCallError> {
    let decided = match reads {
        Some(reads) => Some(reads.may_read(caller, address).await),
        None => None,
    };
    let why = match decided {
        Some(Ok(()) | Err(ActionCallError::ConfirmationRequired(_))) => return Ok(()),
        Some(Err(refusal)) => refusal.to_string(),
        None => "this device serves no reader of records".to_owned(),
    };
    Err(ActionCallError::refused(format!(
        "{} may not read its record `{}`: {why}",
        address.app, address.record
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a test's unwrap is its assertion")]

    use super::*;

    const APP: &str = "so.tilo.tasks";
    const WORKSPACE: &str = "a3b1c2d4-0000-4000-8000-000000000001";

    fn lane() -> StoreLane {
        StoreLane {
            daemon: Weak::new(),
            app: APP.to_owned(),
            workspace: WORKSPACE.to_owned(),
            runs: Mutex::default(),
        }
    }

    fn change(record: Option<&str>, changes: &[u8], more: bool) -> serde_json::Value {
        serde_json::to_value(Command::RecordChange {
            record: record.map(str::to_owned),
            changes: Bytes(changes.to_vec()),
            more,
            commit: None,
        })
        .unwrap()
    }

    fn app() -> Caller {
        Caller::launched(APP).in_workspace(WORKSPACE)
    }

    #[test]
    fn a_change_sent_in_parts_is_taken_whole_in_the_order_it_came() {
        let lane = lane();
        assert!(
            lane.taken(&app(), change(Some("r"), b"ab", true))
                .unwrap()
                .is_none()
        );
        assert!(
            lane.taken(&app(), change(Some("r"), b"cd", true))
                .unwrap()
                .is_none()
        );
        let Some(Command::RecordChange { changes, more, .. }) =
            lane.taken(&app(), change(Some("r"), b"ef", false)).unwrap()
        else {
            panic!("the last part hands the store the whole change");
        };
        assert_eq!((changes.0.as_slice(), more), (&b"abcdef"[..], false));
    }

    #[test]
    fn what_the_connection_may_not_send_is_refused_before_the_store() {
        let lane = lane();
        let elsewhere = Caller::launched(APP).in_workspace("another-workspace");
        assert!(matches!(
            lane.taken(&elsewhere, change(Some("r"), b"x", false)),
            Err(ActionCallError::Refused { .. })
        ));
        let agent = Caller::delegated_agent("scribe", [1; 32], [2; 32]).in_workspace(WORKSPACE);
        assert!(matches!(
            lane.taken(&agent, change(Some("r"), b"x", false)),
            Err(ActionCallError::Refused { .. })
        ));
        assert!(matches!(
            lane.taken(&app(), change(None, b"x", false)),
            Err(ActionCallError::InvalidRequest { .. })
        ));
        let unplaced = StoreLane {
            app: String::new(),
            ..lane
        };
        assert!(matches!(
            unplaced.taken(&app(), change(Some("r"), b"x", false)),
            Err(ActionCallError::Refused { .. })
        ));
    }
}
