//! What the host changed, published as the member, and the workspace's
//! record index kept with it.
//!
//! A workspace holds a record by naming it in its app's index
//! ([`ui::record::index`]): the document every window of the app reads its
//! workspace's records from, and what a person's search finds them by. A
//! window names the record it opens there itself; a record the host makes
//! or changes for someone (an agent's, with no window open) is named there
//! by the host, as the same act, under what its app calls it. Every change
//! the host publishes passes [`Worker::commit`], so that is where it names
//! the record: one place, whoever asked for the change.

use collab_types::DocumentMutation;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Outbox};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{ActorPlace, InvocationReceipt, InvocationState};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use super::{COMMIT_WAIT, Ledger, Needs, Outcome, Performed, Worker, is_part};
use tilo_relay_protocol::WorkspaceId;

/// The record at `route` as the session's replica holds it, and whether the
/// replica holds it at all.
fn document(
    peer: &tilo_session_client::Link,
    route: &DocumentRoute,
) -> (automerge::AutoCommit, bool) {
    match peer
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.record)
    {
        Ok(Some(bytes)) => (
            automerge::AutoCommit::load(&bytes).unwrap_or_else(|error| {
                tracing::warn!(record = %route.record, %error, "workspace actions: a record does not load");
                automerge::AutoCommit::new()
            }),
            true,
        ),
        _ => (automerge::AutoCommit::new(), false),
    }
}

/// How long an invocation waits to hear its writes were saved before it is
/// answered pending: the caller follows it with `tilo_status` until it is
/// saved. A write the host saves at once (most) is answered done directly.
const SAVE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a write answered pending may wait to be saved before it is
/// answered failed: whoever follows it hears how it went within this,
/// whatever holds the save up. The host goes on watching it and logs
/// whether it is saved after all (a write held for its record's keys is
/// saved once they arrive, however late).
const SAVE_WATCH: std::time::Duration = std::time::Duration::from_secs(60);

/// One write the host handed to the session, as its log names it.
pub(super) struct Written {
    app: String,
    workspace: String,
    record: String,
    whole: bool,
    changes: Vec<String>,
}

impl Written {
    pub(super) fn of(route: &DocumentRoute, mutation: &DocumentMutation) -> Self {
        Self {
            app: route.app_id.clone(),
            workspace: route.cluster_id.clone(),
            record: route.record.clone(),
            whole: matches!(mutation, DocumentMutation::Snapshot(_)),
            changes: changes_of(mutation),
        }
    }

    /// The route the write went to.
    pub(super) fn route(&self) -> DocumentRoute {
        DocumentRoute {
            app_id: self.app.clone(),
            cluster_id: self.workspace.clone(),
            record: self.record.clone(),
        }
    }

    fn saved(&self) {
        tracing::info!(
            app = %self.app,
            workspace = %self.workspace,
            record = %self.record,
            whole = self.whole,
            changes = ?self.changes,
            "workspace actions: a change is saved"
        );
    }

    fn unsaved(&self, reason: &str) {
        tracing::warn!(
            app = %self.app,
            workspace = %self.workspace,
            record = %self.record,
            whole = self.whole,
            changes = ?self.changes,
            %reason,
            "workspace actions: a change is not saved"
        );
    }
}

/// Where the writes one request handed to the session stand once it ran
/// ([`Worker::saving`]).
pub(super) enum Saving {
    /// The host holds every one of them durably.
    Saved,
    /// The host refused them, or the session ended before it held them:
    /// why.
    Failed(String),
    /// Not yet saved, as when held for a record's keys: [`Unsaved::settle`]
    /// says how it ends.
    Pending(Unsaved),
}

/// Writes handed to the session that the host has not said it holds yet.
pub(super) struct Unsaved {
    saved: tilo_session_client::Receipt<Result<(), String>>,
    written: Vec<Written>,
}

impl Unsaved {
    /// Writes nobody named, waiting on `saved`: what a test settles.
    #[cfg(test)]
    pub(super) const fn waiting(saved: tilo_session_client::Receipt<Result<(), String>>) -> Self {
        Self {
            saved,
            written: Vec::new(),
        }
    }

    /// Wait, off the service's thread, until the host holds the writes or
    /// refuses them, then `settled` hears which: `Ok` once saved, or why
    /// not. Writes still unsaved after [`SAVE_WATCH`] are answered failed
    /// then, and logged as and when they are saved after all.
    pub(super) fn settle(self, settled: impl FnOnce(Result<(), ActionFailure>) + Send + 'static) {
        self.settle_within(SAVE_WATCH, settled);
    }

    /// [`Self::settle`], answered failed once `bound` passed unsaved.
    pub(super) fn settle_within(
        self,
        bound: std::time::Duration,
        settled: impl FnOnce(Result<(), ActionFailure>) + Send + 'static,
    ) {
        let Self { saved, written } = self;
        let watching = std::thread::Builder::new()
            .name("write-saved".into())
            .spawn(move || {
                let outcome = match saved.recv_timeout(bound) {
                    Ok(outcome) => outcome,
                    Err(tilo_session_client::RecvTimeoutError::Timeout) => {
                        for write in &written {
                            write.unsaved(
                                "not saved yet: it waits for its record's keys or the host",
                            );
                        }
                        settled(Err(unconfirmed_failure()));
                        let outcome = saved.recv().unwrap_or_else(|_| {
                            Err("the session ended before the host saved the change".into())
                        });
                        log_saved(&written, &outcome);
                        return;
                    }
                    Err(tilo_session_client::RecvTimeoutError::Disconnected) => {
                        Err("the session ended before the host saved the change".into())
                    }
                };
                log_saved(&written, &outcome);
                settled(outcome.map_err(|reason| {
                    unsaved_failure(format!("the host did not save the change: {reason}"))
                }));
            });
        if let Err(error) = watching {
            tracing::warn!(%error, "workspace actions: whether a write was saved is not watched");
        }
    }
}

fn log_saved(written: &[Written], outcome: &Result<(), String>) {
    match outcome {
        Ok(()) => written.iter().for_each(Written::saved),
        Err(reason) => written.iter().for_each(|write| write.unsaved(reason)),
    }
}

/// The changes `mutation` carries, short hashes, for the log: each change of
/// a run of them, or the heads of a whole record.
fn changes_of(mutation: &DocumentMutation) -> Vec<String> {
    let hashes = match mutation {
        DocumentMutation::Incremental(bytes) => {
            tilo_automerge_isolate::change_hashes(bytes).unwrap_or_default()
        }
        DocumentMutation::Snapshot(bytes) => automerge::AutoCommit::load(bytes)
            .map(|mut doc| doc.get_heads())
            .unwrap_or_default(),
    };
    hashes
        .iter()
        .map(|hash| hash.to_string().chars().take(12).collect())
        .collect()
}

impl Worker {
    /// Publish `mutation` as the member: sealed with the record's channel
    /// keys, or held until a participant admits the member to it. The write
    /// is kept with what the request being served handed over, which is
    /// answered done only once the host holds it ([`Self::saving`]).
    pub(super) fn commit(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        self.hand_over(workspace_id, route, mutation)?;
        self.handed
            .entry(workspace_id)
            .or_default()
            .push(Written::of(route, mutation));
        // The workspace holds the record by its index naming it, as a
        // window opening the record names it there.
        self.index_record(workspace_id, route);
        Ok(())
    }

    /// Hand `mutation` to the session, which seals it with the record's
    /// channel keys, or holds it until a participant admits the member.
    fn hand_over(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        if !self.workspaces.contains_key(&workspace_id) {
            return Err("the workspace was not joined".into());
        }
        // A record the host starts — a chain's first part, a report — has a
        // channel of its own. The host device opens it and hands the session
        // its keys, as it does for a record it opens, so the first change
        // goes out sealed instead of waiting for keys nobody offers.
        if self.channels_opened.insert(route.clone())
            && let Some(keys) = self
                .shared
                .link
                .get()
                .and_then(|link| link.host_keys(route, false))
        {
            let keys = keys?;
            self.workspaces
                .get_mut(&workspace_id)
                .ok_or("the workspace was not joined")?
                .peer
                .send(Cmd::InstallChannelKeys(route.clone(), keys))?;
        }
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or("the workspace was not joined")?;
        workspace.peer.write(route.clone(), mutation)?;
        let written = Written::of(route, mutation);
        tracing::debug!(
            app = %written.app,
            workspace = %written.workspace,
            record = %written.record,
            whole = written.whole,
            changes = ?written.changes,
            "workspace actions: a change was handed to the session"
        );
        Ok(())
    }

    /// Hand `mutation` to the session, done only once the host holds it
    /// durably (the session's barrier): what the next write is made over.
    fn commit_durably(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        self.hand_over(workspace_id, route, mutation)?;
        let saved = self
            .workspaces
            .get(&workspace_id)
            .ok_or("the workspace was not joined")?
            .peer
            .barrier()?;
        match saved.recv_timeout(COMMIT_WAIT) {
            Ok(Ok(())) => {
                Written::of(route, mutation).saved();
                Ok(())
            }
            Ok(Err(reason)) => Err(format!("the host did not save the change: {reason}")),
            Err(_) => Err("the host has not acknowledged saving the change".into()),
        }
    }

    /// Answer `invocation` pending: it ran, and is done once the host holds
    /// what it wrote, which the caller follows it to.
    pub(super) fn answer_pending(
        &self,
        invocation: InvocationId,
        (output, left_at): (ActionOutput, Option<ActorPlace>),
        unsaved: Unsaved,
    ) -> InvocationReceipt {
        let mut ledger = self.ledger();
        ledger.running = None;
        if let Some(entry) = ledger.entries.get_mut(&invocation) {
            entry.saving = true;
            entry.left_at = left_at;
        }
        drop(ledger);
        let shared = Arc::clone(&self.shared);
        unsaved.settle(move |saved| {
            settle_saved(&shared.ledger, invocation, output, saved);
        });
        InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt()
    }

    /// `outcome`, an action's, once what it wrote is saved: failed when the
    /// host refuses it, and with what is still on its way to be saved when
    /// it is not saved yet. One rule for every caller: done means saved.
    /// What a write not yet saved was made through, a caller's replica
    /// within bounds, is let go, so nothing unsaved is acted on again.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn saved(
        &mut self,
        workspace_id: WorkspaceId,
        outcome: Performed,
    ) -> (Performed, Option<Unsaved>) {
        if outcome.is_err() {
            return (outcome, None);
        }
        let routes: HashSet<DocumentRoute> =
            self.handed.values().flatten().map(Written::route).collect();
        let (outcome, unsaved) = match self.saving(workspace_id) {
            Saving::Saved => return (outcome, None),
            Saving::Failed(reason) => (Err(Outcome::Failed(unsaved_failure(reason))), None),
            Saving::Pending(unsaved) => (outcome, Some(unsaved)),
        };
        let holders: HashSet<DocumentRoute> =
            routes.iter().map(|route| self.holder_of(route)).collect();
        self.bounded
            .retain(|(route, _, _), _| !routes.contains(route) && !holders.contains(route));
        (outcome, unsaved)
    }

    /// Where the writes the request just served handed to the session
    /// stand: saved, refused, or still on their way after [`SAVE_GRACE`].
    /// Every write, whoever it was made for, is answered by this one rule:
    /// done only once the host holds it.
    pub(super) fn saving(&mut self, workspace_id: WorkspaceId) -> Saving {
        let written: Vec<Written> = std::mem::take(&mut self.handed)
            .into_values()
            .flatten()
            .collect();
        if written.is_empty() {
            return Saving::Saved;
        }
        let saved = match self
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| "the workspace was not joined".to_owned())
            .and_then(|workspace| {
                workspace
                    .peer
                    .barrier()
                    .map_err(|closed| closed.to_string())
            }) {
            Ok(saved) => saved,
            Err(reason) => {
                let reason = format!("the host did not save the change: {reason}");
                log_saved(&written, &Err(reason.clone()));
                return Saving::Failed(reason);
            }
        };
        match saved.recv_timeout(SAVE_GRACE) {
            Ok(outcome) => {
                log_saved(&written, &outcome);
                match outcome {
                    Ok(()) => Saving::Saved,
                    Err(reason) => {
                        Saving::Failed(format!("the host did not save the change: {reason}"))
                    }
                }
            }
            Err(tilo_session_client::RecvTimeoutError::Timeout) => {
                Saving::Pending(Unsaved { saved, written })
            }
            Err(tilo_session_client::RecvTimeoutError::Disconnected) => {
                let reason = "the host did not save the change: the session ended".to_owned();
                log_saved(&written, &Err(reason.clone()));
                Saving::Failed(reason)
            }
        }
    }

    /// Log whether the host comes to hold what a job that answers no
    /// invocation handed to the session, without holding the job.
    pub(super) fn watch_handed(&mut self) {
        for (workspace_id, written) in std::mem::take(&mut self.handed) {
            let Some(saved) = self
                .workspaces
                .get(&workspace_id)
                .and_then(|workspace| workspace.peer.barrier().ok())
            else {
                continue;
            };
            Unsaved { saved, written }.settle(|_| {});
        }
    }

    /// What the record at `route` is called in its app's index of the
    /// workspace, when the index names it.
    pub(super) fn record_name(&self, workspace_id: WorkspaceId, route: &DocumentRoute) -> Option<String> {
        let workspace = self.workspaces.get(&workspace_id)?;
        let checkout = ui::record::identity(&document(&workspace.peer, route).0)?;
        let index = DocumentRoute {
            record: String::new(),
            ..route.clone()
        };
        ui::record::index::name_of(&document(&workspace.peer, &index).0, &checkout.id)
    }

    /// Name the record at `route` in its app's index of the workspace,
    /// under what its app calls it now, unless the index already does. A
    /// record of the workspace itself (its desk, its emoji), the index, and
    /// a part of a record's chain are no records of their own there.
    pub(super) fn index_record(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) {
        if route.record.is_empty()
            || is_part(&route.record)
            || collab_types::workspace_record(&route.app_id, &route.record).is_some()
            || self.catalogue.get(&route.app_id).is_none()
        {
            return;
        }
        if let Err(reason) = self.name_in_index(workspace_id, route) {
            tracing::warn!(app = %route.app_id, record = %route.record, %reason,
                "workspace actions: a record is not named in its workspace's index");
        }
    }

    fn name_in_index(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) -> Result<(), String> {
        let index_route = DocumentRoute {
            record: String::new(),
            ..route.clone()
        };
        let link = self.shared.link.get().cloned();
        let mut record = {
            let workspace = self
                .workspaces
                .get(&workspace_id)
                .ok_or("the workspace was not joined")?;
            document(&workspace.peer, route).0
        };
        let Some(checkout) = ui::record::identity(&record) else {
            return Ok(());
        };
        // What the app calls the record is the app's to say: read in its
        // confined worker, as the record's bytes are anyone's.
        let name = match self.titles.title(&route.app_id, record.save()) {
            Ok(name) => name,
            Err(reason) => {
                tracing::info!(app = %route.app_id, %reason,
                    "workspace actions: a record's name is not read");
                None
            }
        };
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or("the workspace was not joined")?;
        // The index is written over what the workspace holds of it, never
        // beside it: its keys first (made when the app has no index yet in
        // this workspace), then everything that arrived under them.
        if let Some(link) = &link
            && let Some(keys) = link.host_keys(&index_route, true)
        {
            workspace
                .peer
                .send(Cmd::InstallChannelKeys(index_route.clone(), keys?))?;
        }
        workspace
            .wait_for(
                workspace_id,
                Needs::Act {
                    app_id: &route.app_id,
                    record: "",
                },
                link.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        let (mut index, held) = document(&workspace.peer, &index_route);
        let _ = index.save();
        let named = ui::record::index::reference(&mut index, &checkout.id, &checkout.kind);
        let renamed = name.is_some()
            && ui::record::index::set_name(&mut index, &checkout.id, name.as_deref());
        if !named && !renamed {
            return Ok(());
        }
        let change = index.save_incremental();
        if change.is_empty() {
            return Ok(());
        }
        // An index the workspace holds nothing of yet starts as the whole
        // record: a reader holding none of it reads it from that, where a
        // change alone would wait for a base no one sends.
        let mutation = if held {
            DocumentMutation::Incremental(change)
        } else {
            DocumentMutation::Snapshot(index.save())
        };
        // Durably: the next name is written over this one, read back from the
        // session's replica.
        self.commit_durably(workspace_id, &index_route, &mutation)
    }
}

/// `invocation`, answered pending while what it wrote was on its way, ends
/// as the save did: done with `output` once the host holds it, or failed
/// with why it does not, or that it was not confirmed in time.
pub(super) fn settle_saved(
    ledger: &Mutex<Ledger>,
    invocation: InvocationId,
    output: ActionOutput,
    saved: Result<(), ActionFailure>,
) {
    let mut ledger = ledger.lock_recover();
    if let Some(entry) = ledger.entries.get_mut(&invocation) {
        entry.saving = false;
        entry.update = match saved {
            Ok(()) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => InvocationUpdate::Failed { invocation, error },
        };
    }
}

/// Why an action whose change the host has not confirmed saving within
/// [`SAVE_WATCH`] failed. The change may still be saved (it may wait for its
/// record's keys), so asking again could make it twice: not retryable, the
/// caller reads the record first.
fn unconfirmed_failure() -> ActionFailure {
    ActionFailure {
        code: "save_unconfirmed".into(),
        message: "the host has not confirmed saving the change within a minute; it may still \
                  be saved, so read the record before making the change again"
            .into(),
        details: None,
        retryable: false,
    }
}

/// Why an action whose change the host did not save failed. The workspace
/// took none of it (it refused the change, or the session it rode ended), so
/// asking again runs it again over the session the workspace holds now.
fn unsaved_failure(message: String) -> ActionFailure {
    ActionFailure {
        code: "commit_failed".into(),
        message,
        details: None,
        retryable: true,
    }
}
