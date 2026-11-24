//! A route's record as the device's store keeps it. The app holds its own copy of the record
//! and nothing else: what it changes goes to the store as its own changes
//! (`tilo_resource_store::wire::Command::RecordChange`), and when the record changes elsewhere
//! the store says so on the route's context feed and the app asks for the changes it lacks
//! (`Command::RecordChanges`), once per notice. Both travel in pages of at most
//! [`RECORD_PAGE_BYTES`]. The store reads, decides, seals and signs; the app holds no key for
//! its record.

use std::str::FromStr as _;

use automerge::ChangeHash;
use collab_types::DocumentMutation;
use tilo_lock::MutexRecover as _;
use tilo_resource_store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply};

use super::{BridgeState, CollabEvent, DocumentPublicationError, InboundBridge, RouteSpec};

/// The heads of everything both the app and the store hold of a route's record: what the app
/// asks past, and what its own accepted changes move on.
#[derive(Clone, Default)]
pub(super) struct Frontier(std::sync::Arc<std::sync::Mutex<Vec<ChangeHash>>>);

impl Frontier {
    fn heads(&self) -> Vec<String> {
        self.0
            .lock_recover()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The store sent everything up to `heads`.
    fn reached(&self, heads: Vec<ChangeHash>) {
        *self.0.lock_recover() = heads;
    }

    /// The store took `changes`: they, and what they build on, are in both.
    fn took(&self, changes: &[automerge::Change]) {
        let mut heads = self.0.lock_recover();
        heads.extend(changes.iter().map(automerge::Change::hash));
        let built_on: std::collections::HashSet<ChangeHash> = changes
            .iter()
            .flat_map(|change| change.deps().iter().copied())
            .collect();
        heads.retain(|head| !built_on.contains(head));
        heads.sort_unstable();
        heads.dedup();
    }
}

/// What following a route's record holds: the task that reads it, ended with the route.
pub(super) struct Follower {
    task: tokio::task::AbortHandle,
    changed: std::rc::Rc<tokio::sync::Notify>,
}

impl Follower {
    /// The store said the record changed: read it again, once for every notice that came
    /// while a read was out.
    pub(super) fn changed(&self) -> std::rc::Rc<tokio::sync::Notify> {
        std::rc::Rc::clone(&self.changed)
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read what the store holds of `spec`'s record past `frontier` now, as the route opens, and
/// again after each notice that it changed, delivering the changes to the app.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) fn follow(
    channel: runtime_collab::ChannelHandle,
    spec: RouteSpec,
    frontier: Frontier,
    inbound_tx: InboundBridge,
) -> Follower {
    let changed = std::rc::Rc::new(tokio::sync::Notify::new());
    let notices = std::rc::Rc::clone(&changed);
    let task = tokio::task::spawn_local(async move {
        loop {
            match read(&channel, &frontier).await {
                Ok(Some(mutation)) => {
                    let event = CollabEvent::Document {
                        route: spec.clone(),
                        mutation,
                    };
                    if !inbound_tx.send_document(event).await {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        app = %spec.app_id,
                        record = spec.document.as_deref().unwrap_or(""),
                        %error,
                        "collab: the record's changes could not be read; read again at its next change"
                    );
                }
            }
            notices.notified().await;
        }
    });
    Follower {
        task: task.abort_handle(),
        changed,
    }
}

/// The changes of the record past `frontier`, joined from every page of one answer, moving the
/// frontier to where they bring it; `None` when there are none. A page naming other heads than
/// the first belongs to a newer answer, which is read again from its start.
async fn read(
    channel: &runtime_collab::ChannelHandle,
    frontier: &Frontier,
) -> Result<Option<DocumentMutation>, String> {
    let after = frontier.heads();
    let (changes, heads) = 'answer: loop {
        let mut changes = Vec::new();
        let mut first: Option<Vec<String>> = None;
        let mut from = 0;
        loop {
            let command = Command::RecordChanges {
                after: after.clone(),
                from,
            };
            let reply = ask(channel, &command)
                .await
                .map_err(|error| error.to_string())?;
            let Reply::RecordChanges {
                changes: page,
                heads,
                next,
            } = reply
            else {
                return Err(format!("the store answered {reply:?}"));
            };
            match &first {
                Some(first) if *first != heads => continue 'answer,
                Some(_) => {}
                None => first = Some(heads),
            }
            changes.extend(page.0);
            match next {
                Some(next) => from = next,
                None => break 'answer (changes, first.unwrap_or_default()),
            }
        }
    };
    let heads = heads
        .iter()
        .map(|head| ChangeHash::from_str(head).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    frontier.reached(heads);
    Ok((!changes.is_empty()).then_some(DocumentMutation::Incremental(changes)))
}

/// Hand the store `mutation`, the app's own changes of `route`'s record, as the changes it
/// carries: a record's first write is every change it holds, from none. They go in runs of at
/// most [`RECORD_PAGE_BYTES`], in order, each but the last with `more`; every run names
/// `commit`, the action they are one part of. Every failure is recorded on the bridge before
/// this returns; a refusal is the store's answer about these changes alone, and the bridge
/// stays open.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) async fn write(
    channel: &runtime_collab::ChannelHandle,
    route: &RouteSpec,
    frontier: &Frontier,
    mutation: &DocumentMutation,
    commit: Option<collab_types::WorkspaceCommit>,
    state: &BridgeState,
) -> Result<(), DocumentPublicationError> {
    // Bytes that are no change are the app's own fault, about this change alone.
    let changes = collab_types::parse_changes(mutation.data())
        .map_err(|error| state.publication_failed("record change", error, false))?;
    let bytes: Vec<u8> = changes
        .iter()
        .flat_map(|change| change.raw_bytes().iter().copied())
        .collect();
    let runs: Vec<&[u8]> = bytes.chunks(RECORD_PAGE_BYTES).collect();
    for (index, run) in runs.iter().enumerate() {
        let command = Command::RecordChange {
            changes: Bytes(run.to_vec()),
            more: index + 1 < runs.len(),
            commit: commit.clone(),
        };
        match ask(channel, &command).await {
            Ok(Reply::Written) => {}
            Ok(other) => {
                return Err(state.publication_failed(
                    "record change",
                    format!("the store answered {other:?}"),
                    true,
                ));
            }
            Err(Asked::Refused(error)) => {
                state.note_refused_change(route, "record change", &error);
                return Err(DocumentPublicationError::Refused(format!(
                    "record change: {error}"
                )));
            }
            Err(error) => return Err(state.publication_failed("record change", error, true)),
        }
    }
    frontier.took(&changes);
    state.change_sent(route);
    Ok(())
}

/// Why a store command was not answered.
enum Asked {
    /// The store read it and would not take it.
    Refused(String),
    /// No answer, or one that does not read: the outcome is unknown.
    Lost(String),
}

impl std::fmt::Display for Asked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) | Self::Lost(reason) => f.write_str(reason),
        }
    }
}

async fn ask(channel: &runtime_collab::ChannelHandle, command: &Command) -> Result<Reply, Asked> {
    let bytes = serde_json::to_vec(command).map_err(|error| Asked::Lost(error.to_string()))?;
    let reply = channel
        .resource_command(&bytes)
        .await
        .map_err(|error| match error {
            runtime_portal::NotAvailable::Refused(reason) => Asked::Refused(reason),
            error => Asked::Lost(error.to_string()),
        })?;
    serde_json::from_slice(&reply).map_err(|error| Asked::Lost(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    #[test]
    fn the_frontier_moves_past_what_the_store_took() {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "a", 1).unwrap();
        doc.commit();
        let first = doc.get_heads();
        doc.put(automerge::ROOT, "a", 2).unwrap();
        doc.commit();
        let frontier = Frontier::default();
        frontier.reached(first.clone());
        let taken = collab_types::parse_changes(&doc.save_after(&first)).unwrap();
        frontier.took(&taken);
        assert_eq!(
            frontier.heads(),
            doc.get_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            "the change the store took replaces the head it built on"
        );
    }
}
