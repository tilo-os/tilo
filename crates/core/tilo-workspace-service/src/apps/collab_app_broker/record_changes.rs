//! The record an app's process opened, reached through the device's one store: the process asks
//! the record's changes past what it holds (`RecordChanges`, in pages) and hands the device its
//! own (`RecordChange`, in runs it takes as one write), and is told when the record changed
//! (`ContextUpdate::RecordChanged`). The process holds no key: the device's workspace service
//! reads and writes the record, and this decides each read for the channel's caller as the
//! device's one reader does.

#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

use tilo_resource_store::wire::{Bytes, Command, Reply};

/// The runs of each record's change still to come on one channel, by the record they name.
pub(super) type RecordRuns = RefCell<std::collections::HashMap<Option<String>, Vec<u8>>>;

/// `command` as the store takes it: a change of the record whole. A run of the change with more
/// to come is kept in `runs` (`None`), and the last run is answered with every run before it of
/// the same record.
pub(super) fn gathered(runs: &RecordRuns, command: Command) -> Option<Command> {
    let Command::RecordChange {
        record,
        changes,
        more,
        commit,
    } = command
    else {
        return Some(command);
    };
    let mut runs = runs.borrow_mut();
    let gathering = runs.entry(record.clone()).or_default();
    gathering.extend_from_slice(&changes.0);
    if more {
        return None;
    }
    let changes = runs.remove(&record).unwrap_or_default();
    Some(Command::RecordChange {
        record,
        changes: Bytes(changes),
        more: false,
        commit,
    })
}

/// The route `command` addresses: `route` naming the record a record command names, which is one
/// of the route's app in its workspace; `route` itself for every other command.
pub(super) fn addressed(command: &Command, route: &CrdtRoute) -> CrdtRoute {
    match command {
        Command::RecordChanges {
            record: Some(record),
            ..
        }
        | Command::RecordChange {
            record: Some(record),
            ..
        } => CrdtRoute {
            document_id: record.clone(),
            ..route.clone()
        },
        _ => route.clone(),
    }
}

impl JoinedChannel {
    /// Why the route's app may not found its record, where this device governs the workspace:
    /// a new record is of a kind its installed manifest declares.
    pub(super) fn founding_refusal(&self, route: &CrdtRoute) -> Option<String> {
        let allows = self.state.services.record_founding.as_ref()?;
        let app = &route.app_id;
        let Some(kind) = self.resource_type.as_deref() else {
            return Some(format!("{app} named no kind for a new record"));
        };
        (!allows(app, kind)).then(|| format!("{app} does not declare the resource kind `{kind}`"))
    }

    /// The record a record command of `command` addresses (`route`, [`addressed`]): once the
    /// device's one reader let this channel's caller (`admitted`) read it. `None` for any other
    /// command. A listing of the app's records names none, and each is read by id.
    pub(super) fn record_of(
        &self,
        command: &Command,
        admitted: &AdmittedJoinIdentity,
        route: &CrdtRoute,
    ) -> Result<Option<String>, BrokerError> {
        match command {
            Command::RecordChanges { .. } => {
                self.may_read_record(admitted, route)?;
                Ok(Some(route.document_id.clone()))
            }
            // Decided as every write on the route is, before it got here.
            Command::RecordChange { .. } => Ok(Some(route.document_id.clone())),
            _ => Ok(None),
        }
    }

    /// Refuse a read of the route's record the device's one reader does not let the caller make.
    /// One it would ask the person about reads, as the channel's join asked them already.
    fn may_read_record(
        &self,
        admitted: &AdmittedJoinIdentity,
        route: &CrdtRoute,
    ) -> Result<(), BrokerError> {
        use tilo_actions::remote::ActionCallError::ConfirmationRequired;
        let address = tilo_actions::remote::ResourceAddress {
            workspace: route.cluster_id.clone(),
            app: route.app_id.clone(),
            record: route.document_id.clone(),
        };
        let caller = admitted.certified_caller(&route.cluster_id, false);
        let allowed = self.state.records().is_some_and(|reads| {
            matches!(
                reads
                    .may_read(&caller, &address)
                    .wait_timeout(Duration::MAX),
                Ok(Some(Ok(()) | Err(ConfirmationRequired(_))))
            )
        });
        if allowed {
            Ok(())
        } else {
            Err(BrokerError::failed(format!(
                "this connection may not read the record of {}",
                route.app_id
            )))
        }
    }

    /// `answer`, the store's answer to this channel's own change of `route`'s record: once it is
    /// written, every other window open on the record is told it changed, and search reads it
    /// again.
    pub(super) fn telling_record_changed(
        &self,
        answer: crate::apps::services::ResourceAnswer,
        route: CrdtRoute,
    ) -> crate::apps::services::ResourceAnswer {
        let (state, writer) = (Arc::clone(&self.state), self.connection_id);
        Box::pin(async move {
            let reply = answer.await?;
            if matches!(reply, Reply::Written) {
                state.record_changed(&route, writer);
                if let Some(index) = &state.services.document_index_changes {
                    index.changed(&route.cluster_id, &route.app_id, &route.document_id);
                }
            }
            Ok(reply)
        })
    }
}

impl AppBrokerState {
    /// Tell every window open on `route`'s record but `except` that the record changed: each asks
    /// its changes from what it holds. A committed read waiting on the record wakes as well.
    pub(crate) fn record_changed(&self, route: &CrdtRoute, except: u64) {
        if route.cluster_id.is_empty() {
            return;
        }
        self.wake_committed_reads(route);
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        let channels = channels
            .iter_mut()
            .filter(|(key, _)| route.delivers_to(&CrdtRoute::from_channel_key(key)))
            .map(|(_, subscribers)| subscribers);
        subscriptions::context_frame(channels, except, &ContextUpdate::RecordChanged);
    }
}
