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

use collab_types::store::wire::{Bytes, Command, Reply};

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
    /// Why the route's app may not found its record, where this device hosts the workspace:
    /// a new record is of a kind its installed manifest declares.
    pub(super) fn founding_refusal(&self, route: &CrdtRoute) -> Option<String> {
        self.state
            .founding_refusal(&route.app_id, self.resource_type.as_deref())
    }

    /// The record a record command of `command` addresses (`route`, [`addressed`]): once the
    /// device's one reader let this channel's caller (`admitted`) read it. `None` for any other
    /// command. A listing of the app's records names none, and each is read by id.
    pub(super) fn record_of(
        &self,
        command: &Command,
        admitted: &AdmittedJoinIdentity,
        route: &CrdtRoute,
    ) -> impl std::future::Future<Output = Result<Option<String>, BrokerError>> + Send + use<> {
        let (permitted, record) = match command {
            Command::RecordChanges { .. } => (
                Some(self.may_read_record(admitted, route)),
                Some(route.document_id.clone()),
            ),
            // Decided as every write on the route is, before it got here.
            Command::RecordChange { .. } => (None, Some(route.document_id.clone())),
            _ => (None, None),
        };
        async move {
            if let Some(permitted) = permitted {
                permitted.await?;
            }
            Ok(record)
        }
    }

    /// Refuse a read of the route's record the device's one reader does not let the caller make.
    /// One it would ask the person about reads, as the channel's join asked them already.
    fn may_read_record(
        &self,
        admitted: &AdmittedJoinIdentity,
        route: &CrdtRoute,
    ) -> impl std::future::Future<Output = Result<(), BrokerError>> + Send + use<> {
        use tilo_actions::remote::ActionCallError::ConfirmationRequired;
        let address = session::route_address(route);
        let caller = self.record_caller(admitted, route);
        let reads = self.state.records();
        let app_id = route.app_id.clone();
        let document_id = route.document_id.clone();
        async move {
            let decided = match reads {
                Some(reads) => Some(reads.may_read(&caller, &address).await),
                None => None,
            };
            let why = match decided {
                Some(Ok(()) | Err(ConfirmationRequired(_))) => return Ok(()),
                Some(Err(refusal)) => refusal.to_string(),
                None => "this device serves no reader of records".to_owned(),
            };
            Err(BrokerError::failed(format!(
                "this connection may not read {app_id}'s record `{document_id}`: {why}"
            )))
        }
    }
}

/// `answer`, the store's answer to a channel's own change of `route`'s record: once it is
/// written, every other window open on the record in `state` is told it changed, but `writer`.
pub(super) fn telling_record_changed(
    state: Arc<AppBrokerState>,
    writer: u64,
    answer: crate::services::ResourceAnswer,
    route: CrdtRoute,
) -> crate::services::ResourceAnswer {
    Box::pin(async move {
        let reply = answer.await?;
        if matches!(reply, Reply::Written) {
            state.record_changed(&route, writer);
        }
        Ok(reply)
    })
}

impl AppBrokerState {
    /// Why `app` may not found a record of `kind` in a workspace this device hosts: a new record
    /// is of a kind its installed manifest declares.
    pub(crate) fn founding_refusal(&self, app: &str, kind: Option<&str>) -> Option<String> {
        let allows = self.services.record_founding.as_ref()?;
        let Some(kind) = kind else {
            return Some(format!("{app} named no kind for a new record"));
        };
        (!allows(app, kind)).then(|| format!("{app} does not declare the resource kind `{kind}`"))
    }

    /// Tell every window open on `route`'s record but `except` that the record changed: each asks
    /// its changes from what it holds. A committed read waiting on the record wakes as well.
    pub(crate) fn record_changed(&self, route: &CrdtRoute, except: u64) {
        if route.cluster_id.is_empty() {
            return;
        }
        // A change of the store names the document it wrote: an app's record is the content
        // document of its resource, and the windows open on the record are the ones told.
        let record;
        let route = match tilo_resource_store::content_resource(&route.document_id) {
            Some((_, id)) => {
                record = CrdtRoute {
                    document_id: id.to_owned(),
                    ..route.clone()
                };
                &record
            }
            None => route,
        };
        self.wake_committed_reads(route);
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        let channels = channels
            .iter_mut()
            .filter(|(key, _)| route.delivers_to(&CrdtRoute::from_channel_key(key)))
            .map(|(_, subscribers)| subscribers);
        // A window that went away is no longer shown by this record.
        if subscriptions::context_frame(channels, except, &ContextUpdate::RecordChanged) {
            self.windows_changed();
        }
    }
}
