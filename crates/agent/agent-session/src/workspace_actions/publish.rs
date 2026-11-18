//! What the host changed, published as the member, and the workspace's
//! record index kept with it.
//!
//! A workspace holds a record by naming it in its app's index
//! ([`ui::resource::index`]): the document every window of the app reads its
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
use std::time::{Duration, Instant};

use tilo_actions::remote::{ActorPlace, InvocationReceipt, InvocationState};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use super::{InvocationLog, Job, Needs, Outcome, Performed, Shared, Worker};
use tilo_server_protocol::WorkspaceId;

/// The record at `route` as the session's replica holds it, and whether the
/// replica holds it at all.
fn document(
    peer: &tilo_session_client::Link,
    route: &DocumentRoute,
) -> (automerge::AutoCommit, bool) {
    match peer
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.document)
    {
        Ok(Some(bytes)) => (
            automerge::AutoCommit::load(&bytes).unwrap_or_else(|error| {
                tracing::warn!(record = %route.document, %error, "workspace actions: a record does not load");
                automerge::AutoCommit::new()
            }),
            true,
        ),
        _ => (automerge::AutoCommit::new(), false),
    }
}

/// How long an invocation waits on the service's thread for its writes to be
/// saved before it is answered pending: the caller follows it with
/// `tilo_status` until it is saved.
///
/// The wait is bounded tightly because the thread is the service's throughput:
/// it serves every workspace of its share, and a thread held while one
/// workspace's write reaches stable storage is time none of the others is
/// served in. A write the host confirms at once (the ordinary case) is still
/// answered done; anything slower is answered pending, and the caller follows
/// it to the same outcome.
pub(super) const SAVE_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

/// How often the thread tries the full [`SAVE_GRACE`] again after a save
/// outlived it, so a workspace whose saves got fast again answers done in one
/// round trip.
const SAVE_PROBE: u32 = 64;

/// The least a save's measured pace is padded by, so a confirmation that takes
/// as long as the last one still answers the caller done.
const SAVE_MARGIN: std::time::Duration = std::time::Duration::from_millis(2);

/// How long a write answered pending may wait to be saved before it is
/// answered failed: whoever follows it hears how it went within this,
/// whatever holds the save up. The host goes on watching it and logs
/// whether it is saved after all (a write held for its record's keys is
/// saved once they arrive, however late).
const SAVE_WATCH: std::time::Duration = std::time::Duration::from_secs(60);

/// How often a write still not saved after [`SAVE_WATCH`] is said again in the log, with what
/// the session says it waits for and for how long, while its own document is what the session
/// keeps: a write that only waits behind another document's says so once.
const SAVE_SAID_AGAIN: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the session is given to say what it keeps: it answers from what it holds.
const KEPT_ASK: std::time::Duration = std::time::Duration::from_secs(5);

/// The most documents one line names of what the session keeps.
const MOST_KEPT_SAID: usize = 4;

/// What the session says it still keeps of the workspaces a request wrote to, asked when its
/// writes stay unsaved: in the log's words, and whether a document the request wrote is among
/// them. `None` where it keeps nothing, or does not answer.
type KeptAsk = Box<dyn Fn() -> Option<(String, bool)> + Send>;

/// How long a caller's next action waits for the replica set aside while
/// its last write was saved. Long enough for a save that outlived
/// [`SAVE_GRACE`], short enough that the service's thread, which answers
/// every request of its workspaces, is not held long by one: a save that
/// outlives it lets the replica go and the record is opened again.
pub(super) const REUSE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

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
            record: route.document.clone(),
            whole: matches!(mutation, DocumentMutation::Snapshot(_)),
            changes: changes_of(mutation),
        }
    }

    /// The changes it carries, short hashes as History lists them.
    pub(super) fn changes(&self) -> &[String] {
        &self.changes
    }

    /// The route the write went to.
    pub(super) fn route(&self) -> DocumentRoute {
        DocumentRoute {
            app_id: self.app.clone(),
            cluster_id: self.workspace.clone(),
            document: self.record.clone(),
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
    /// What says why they are not saved, once they stay so ([`Worker::asks_what_is_kept`]).
    asking: Option<KeptAsk>,
    written: Vec<Written>,
    /// Who hears whether the writes were saved, besides the caller
    /// ([`Self::watched`]).
    heard: Vec<Heard>,
}

/// Whether writes still on their way were saved, for whoever waits on it.
pub(super) struct SaveWatch {
    saved: Mutex<Option<bool>>,
    settled: std::sync::Condvar,
}

impl SaveWatch {
    #[must_use]
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            saved: Mutex::new(None),
            settled: std::sync::Condvar::new(),
        })
    }

    pub(super) fn set(&self, saved: bool) {
        self.saved.lock_recover().get_or_insert(saved);
        self.settled.notify_all();
    }

    /// How the writes ended, once they have: `None` while they are on their
    /// way.
    pub(super) fn outcome(&self) -> Option<bool> {
        *self.saved.lock_recover()
    }

    /// Wait at most `bound` for the writes to settle: whether they were saved.
    pub(super) fn wait(&self, bound: std::time::Duration) -> bool {
        let (outcome, _) = self
            .settled
            .wait_timeout_while(self.saved.lock_recover(), bound, |outcome| {
                outcome.is_none()
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = outcome.unwrap_or(false);
        drop(outcome);
        saved
    }
}

/// A [`SaveWatch`] told how its writes ended; one never told counts them
/// not saved.
struct Heard(Arc<SaveWatch>);

impl Drop for Heard {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl Unsaved {
    /// Writes nobody named, waiting on `saved`: what a test settles.
    #[cfg(test)]
    pub(super) const fn waiting(saved: tilo_session_client::Receipt<Result<(), String>>) -> Self {
        Self {
            saved,
            asking: None,
            written: Vec::new(),
            heard: Vec::new(),
        }
    }

    /// These writes, with `asking` saying what the session keeps: what a test answers itself.
    #[cfg(test)]
    pub(super) fn asked_by(
        mut self,
        asking: impl Fn() -> Option<(String, bool)> + Send + 'static,
    ) -> Self {
        self.asking = Some(Box::new(asking));
        self
    }

    /// Hear whether these writes are saved: told once [`Self::settle`] knows,
    /// and not saved when nobody settles them.
    pub(super) fn watched(&mut self) -> Arc<SaveWatch> {
        let watch = SaveWatch::new();
        self.heard.push(Heard(Arc::clone(&watch)));
        watch
    }

    /// Wait, off the service's thread, until the host holds the writes or
    /// refuses them, then `settled` hears which: `Ok` once saved, or why
    /// not. Writes still unsaved after [`SAVE_WATCH`] are answered failed
    /// then, with what the session says it keeps and waits for, which the
    /// log names too, again every [`SAVE_SAID_AGAIN`] while one of their own
    /// documents is kept; and logged as and when they are saved after all.
    pub(super) fn settle(self, settled: impl FnOnce(Result<(), ActionFailure>) + Send + 'static) {
        self.settle_within(SAVE_WATCH, settled);
    }

    /// [`Self::settle`], answered failed once `bound` passed unsaved.
    pub(super) fn settle_within(
        self,
        bound: std::time::Duration,
        settled: impl FnOnce(Result<(), ActionFailure>) + Send + 'static,
    ) {
        let Self {
            saved,
            asking,
            written,
            heard,
        } = self;
        let watching = std::thread::Builder::new()
            .name("write-saved".into())
            .spawn(move || {
                let outcome = match saved.recv_timeout(bound) {
                    Ok(outcome) => outcome,
                    Err(tilo_session_client::RecvTimeoutError::Timeout) => {
                        let kept = say_unsaved(&written, asking.as_ref());
                        drop(heard);
                        settled(Err(unconfirmed_failure(
                            kept.as_ref().map(|(kept, _)| kept.as_str()),
                        )));
                        let mut again = kept.is_some_and(|(_, own)| own);
                        let outcome = loop {
                            match saved.recv_timeout(SAVE_SAID_AGAIN) {
                                Ok(outcome) => break outcome,
                                Err(tilo_session_client::RecvTimeoutError::Timeout) => {
                                    if again {
                                        again = say_unsaved(&written, asking.as_ref())
                                            .is_some_and(|(_, own)| own);
                                    }
                                }
                                Err(tilo_session_client::RecvTimeoutError::Disconnected) => {
                                    break Err(
                                        "the session ended before the host saved the change".into(),
                                    );
                                }
                            }
                        };
                        log_saved(&written, &outcome);
                        return;
                    }
                    Err(tilo_session_client::RecvTimeoutError::Disconnected) => {
                        Err("the session ended before the host saved the change".into())
                    }
                };
                log_saved(&written, &outcome);
                for heard in &heard {
                    heard.0.set(outcome.is_ok());
                }
                settled(outcome.map_err(|reason| {
                    unsaved_failure(format!("the host did not save the change: {reason}"))
                }));
            });
        if let Err(error) = watching {
            tracing::warn!(%error, "workspace actions: whether a write was saved is not watched");
        }
    }
}

/// Log each of `written` as not saved yet, with what the session says it keeps and waits for
/// where it says: its words, and whether a document of these writes is among what it keeps.
fn say_unsaved(written: &[Written], asking: Option<&KeptAsk>) -> Option<(String, bool)> {
    let kept = asking.and_then(|ask| ask());
    let reason = kept.as_ref().map_or_else(
        || "not saved yet: it waits for its record's keys or the host".to_owned(),
        |(kept, _)| format!("not saved yet: the session keeps {kept}"),
    );
    for write in written {
        write.unsaved(&reason);
    }
    kept
}

/// What `peer`'s session says it still keeps of what it wrote to `workspaces`, each document
/// with what its writes wait for and for how long, and whether one of `own` is among them.
fn kept_said(
    peer: &tilo_session_client::Link,
    workspaces: &[WorkspaceId],
    own: &HashSet<DocumentRoute>,
) -> Option<(String, bool)> {
    let kept = peer.kept_of(workspaces).ok()?.recv_timeout(KEPT_ASK).ok()?;
    if kept.is_empty() {
        return None;
    }
    let mut said: Vec<String> = kept
        .iter()
        .take(MOST_KEPT_SAID)
        .map(|kept| {
            format!(
                "{}/{}, which waits for {}, for {} s",
                kept.route.app_id,
                kept.route.document,
                kept.waits,
                kept.age.as_secs()
            )
        })
        .collect();
    if kept.len() > said.len() {
        said.push(format!(
            "and {} more",
            kept.len().saturating_sub(said.len())
        ));
    }
    let theirs = kept.iter().any(|kept| own.contains(&kept.route));
    Some((said.join("; "), theirs))
}

fn log_saved(written: &[Written], outcome: &Result<(), String>) {
    match outcome {
        Ok(()) => written.iter().for_each(Written::saved),
        Err(reason) => written.iter().for_each(|write| write.unsaved(reason)),
    }
}

/// The changes `mutation` carries, short hashes, for the log: each change of
/// an invocation of them, or the heads of a whole record.
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
    /// keys, or held until a member admits the member to it. The write
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
        self.index_document(workspace_id, route);
        Ok(())
    }

    /// Hand `mutation` to the session, which seals it with the record's
    /// channel keys, or holds it until a member admits the member.
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
                .send(Cmd::InstallGroupKeys(route.clone(), keys))?;
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

    /// Answer `invocation` pending: it ran, and is done once the host holds
    /// what it wrote, which the caller follows it to.
    pub(super) fn answer_pending(
        &self,
        invocation: InvocationId,
        (output, left_at): (ActionOutput, Option<ActorPlace>),
        unsaved: Unsaved,
    ) -> InvocationReceipt {
        let mut log = self.log();
        log.running.remove(&invocation);
        if let Some(entry) = log.entries.get_mut(&invocation) {
            entry.saving = true;
            entry.left_at = left_at;
        }
        drop(log);
        let shared = Arc::clone(&self.shared);
        unsaved.settle(move |saved| {
            settle_saved(&shared.log, invocation, output, saved);
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
        self.wrote.extend(routes.iter().cloned());
        let (outcome, mut unsaved) = match self.saving(workspace_id) {
            Saving::Saved => return (outcome, None),
            Saving::Failed(reason) => (Err(Outcome::Failed(unsaved_failure(reason))), None),
            Saving::Pending(unsaved) => (outcome, Some(unsaved)),
        };
        let holders: HashSet<DocumentRoute> =
            routes.iter().map(|route| self.holder_of(route)).collect();
        let made: Vec<_> = self
            .bounded
            .keys()
            .filter(|(route, _, _)| routes.contains(route) || holders.contains(route))
            .cloned()
            .collect();
        // Set aside until the host holds what they wrote, and let go if it
        // does not: a large write outlives the grace, and opening the
        // record again for the caller's next action costs a read of all of
        // it, more with every change it holds.
        let watch = unsaved.as_mut().map(Unsaved::watched);
        for key in made {
            if let (Some(held), Some(watch)) = (self.bounded.remove(&key), &watch) {
                self.parked.park(key, held, Arc::clone(watch));
            }
        }
        (outcome, unsaved)
    }

    /// Where the writes the request just served handed to the session
    /// stand: saved, refused, or still on their way after [`SAVE_GRACE`].
    /// Every write, whoever it was made for, is answered by this one rule:
    /// done only once the host holds it.
    pub(super) fn saving(&mut self, workspace_id: WorkspaceId) -> Saving {
        let handed = std::mem::take(&mut self.handed);
        let workspaces: Vec<WorkspaceId> = handed.keys().copied().collect();
        let written: Vec<Written> = handed.into_values().flatten().collect();
        if written.is_empty() {
            return Saving::Saved;
        }
        let waiting = super::timing_start();
        // Asked of the documents the request wrote and of no other: the service's one session
        // carries every workspace's writes, and a write that waits beside these, in another
        // workspace or for another document of this one, says nothing about them.
        let documents: Vec<DocumentRoute> = written.iter().map(Written::route).collect();
        let saved = match self
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| "the workspace was not joined".to_owned())
            .and_then(|workspace| {
                workspace
                    .peer
                    .barrier_of_documents(&documents)
                    .map_err(|closed| closed.to_string())
            }) {
            Ok(saved) => saved,
            Err(reason) => {
                let reason = format!("the host did not save the change: {reason}");
                log_saved(&written, &Err(reason.clone()));
                super::timing_done("action.save_failed", waiting);
                return Saving::Failed(reason);
            }
        };
        // Waited for as long as the saves this thread served recently have
        // been taking, not the whole grace: a save slower than the grace never
        // held this thread — the thread every workspace of its share is served
        // on — for time it could not use. Probing the full grace now and then
        // learns when saves got fast again.
        let grace = if self.save_probes >= SAVE_PROBE {
            SAVE_GRACE
        } else {
            self.save_grace
        };
        let waited = std::time::Instant::now();
        match saved.recv_timeout(grace) {
            Ok(outcome) => {
                self.save_grace = waited.elapsed().saturating_add(SAVE_MARGIN).min(SAVE_GRACE);
                self.save_probes = 0;
                log_saved(&written, &outcome);
                super::timing_done("action.save", waiting);
                tracing::debug!(
                    writes = written.len(),
                    waited_ms = waited.elapsed().as_millis(),
                    saved = outcome.is_ok(),
                    "workspace actions: a change's save settled on the service thread"
                );
                match outcome {
                    Ok(()) => Saving::Saved,
                    Err(reason) => {
                        Saving::Failed(format!("the host did not save the change: {reason}"))
                    }
                }
            }
            Err(tilo_session_client::RecvTimeoutError::Timeout) => {
                self.save_grace = std::time::Duration::ZERO;
                self.save_probes = 1;
                super::timing_done("action.save_pending", waiting);
                tracing::debug!(
                    writes = written.len(),
                    waited_ms = waited.elapsed().as_millis(),
                    "workspace actions: a change's save outlived the grace; answered pending"
                );
                Saving::Pending(Unsaved {
                    saved,
                    asking: self.asks_what_is_kept(workspace_id, &workspaces, &written),
                    written,
                    heard: Vec::new(),
                })
            }
            Err(tilo_session_client::RecvTimeoutError::Disconnected) => {
                let reason = "the host did not save the change: the session ended".to_owned();
                log_saved(&written, &Err(reason.clone()));
                super::timing_done("action.save_failed", waiting);
                Saving::Failed(reason)
            }
        }
    }

    /// Log whether the host comes to hold what a job that answers no
    /// invocation handed to the session, without holding the job.
    pub(super) fn watch_handed(&mut self) {
        for (workspace_id, written) in std::mem::take(&mut self.handed) {
            let documents: Vec<DocumentRoute> = written.iter().map(Written::route).collect();
            let Some(saved) = self
                .workspaces
                .get(&workspace_id)
                .and_then(|workspace| workspace.peer.barrier_of_documents(&documents).ok())
            else {
                continue;
            };
            Unsaved {
                saved,
                asking: self.asks_what_is_kept(workspace_id, &[workspace_id], &written),
                written,
                heard: Vec::new(),
            }
            .settle(|_| {});
        }
    }

    /// What asks the session of the workspace `workspace_id` what it keeps of `workspaces`,
    /// once `written` stays unsaved.
    fn asks_what_is_kept(
        &self,
        workspace_id: WorkspaceId,
        workspaces: &[WorkspaceId],
        written: &[Written],
    ) -> Option<KeptAsk> {
        let peer = self.workspaces.get(&workspace_id)?.peer.clone();
        let workspaces = workspaces.to_vec();
        let own: HashSet<DocumentRoute> = written.iter().map(Written::route).collect();
        Some(Box::new(move || kept_said(&peer, &workspaces, &own)))
    }

    /// What the record at `route` is called in its app's index of the
    /// workspace, when the index names it.
    pub(super) fn document_name(
        &self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
    ) -> Option<String> {
        let workspace = self.workspaces.get(&workspace_id)?;
        let checkout = ui::resource::identity(&document(&workspace.peer, route).0)?;
        let index = DocumentRoute {
            document: String::new(),
            ..route.clone()
        };
        ui::resource::index::name_of(&document(&workspace.peer, &index).0, &checkout.id)
    }

    /// Name the record at `route` in its app's index of the workspace,
    /// under what its app calls it now, unless the index already does. A
    /// record of the workspace itself (its layout, its emoji), the index, and
    /// a record kept in another's tree (a part of its chain, a chunk of it, a
    /// child of it) are no records of their own there.
    ///
    /// What an app calls a record is read from the whole record, so it is not
    /// read on every write: a record this service has not named yet is named
    /// at once (an answer that says done means the index lists it), and a
    /// record written since is named again once, [`RENAME_AFTER`] after it
    /// last was, however many writes it took meanwhile.
    pub(super) fn index_document(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) {
        if route.document.is_empty()
            || in_a_tree(&route.document)
            || collab_types::workspace_document(&route.app_id, &route.document).is_some()
            || self.catalogue.get(&route.app_id).is_none()
        {
            return;
        }
        match self.named.get(route) {
            Some(named) => {
                let wait = RENAME_AFTER.saturating_sub(named.elapsed());
                queue_rename(&self.shared, workspace_id, route, wait);
            }
            None => self.rename(workspace_id, route),
        }
    }

    /// Read what the record at `route` is called and name it so in its app's
    /// index ([`Self::index_document`]).
    pub(super) fn rename(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) {
        self.shared.renaming.lock_recover().remove(route);
        self.named.insert(route.clone(), Instant::now());
        if let Err(reason) = self.name_in_index(workspace_id, route) {
            tracing::warn!(app = %route.app_id, record = %route.document, %reason,
                "workspace actions: a record is not named in its workspace's index");
        }
    }

    fn name_in_index(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
    ) -> Result<(), String> {
        let index_route = DocumentRoute {
            document: String::new(),
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
        let Some(checkout) = ui::resource::identity(&record) else {
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
                .send(Cmd::InstallGroupKeys(index_route.clone(), keys?))?;
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
        let named = ui::resource::index::reference(&mut index, &checkout.id, &checkout.kind);
        let renamed = name.is_some()
            && ui::resource::index::set_name(&mut index, &checkout.id, name.as_deref());
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
        // Handed to the session like the change it names, so the barrier the
        // request's answer waits on covers it: done means the host holds the
        // record and the index that names it.
        self.commit(workspace_id, &index_route, &mutation)
    }
}

/// Whether `record` is kept in another record's tree: a part of its chain, a
/// chunk of it, a child of it.
fn in_a_tree(record: &str) -> bool {
    ui::resource::DocumentId::parse(record).is_some_and(|id| id.tree().is_some())
}

/// How long after a record's name was read it is read again when the record
/// took more writes: a burst of writes reads it once, not once each.
const RENAME_AFTER: Duration = Duration::from_secs(2);

/// Have the record at `route` named again in `wait`, unless that is asked
/// already ([`Worker::index_document`]).
fn queue_rename(
    shared: &Arc<Shared>,
    workspace_id: WorkspaceId,
    route: &DocumentRoute,
    wait: Duration,
) {
    if !shared.renaming.lock_recover().insert(route.clone()) {
        return;
    }
    let queue = move |shared: &Shared, route: DocumentRoute| {
        let sent = shared.jobs.lock_recover().as_ref().is_some_and(|jobs| {
            jobs.send((Instant::now(), Job::Rename(workspace_id, route.clone())))
                .is_ok()
        });
        if !sent {
            shared.renaming.lock_recover().remove(&route);
        }
    };
    if wait.is_zero() {
        queue(shared, route.clone());
        return;
    }
    let (later, route) = (Arc::clone(shared), route.clone());
    let spawned = std::thread::Builder::new()
        .name("record-rename".into())
        .spawn(move || {
            std::thread::sleep(wait);
            queue(&later, route);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "workspace actions: a record's name is not read again");
    }
}

/// `invocation`, answered pending while what it wrote was on its way, ends
/// as the save did: done with `output` once the host holds it, or failed
/// with why it does not, or that it was not confirmed in time.
pub(super) fn settle_saved(
    log: &Mutex<InvocationLog>,
    invocation: InvocationId,
    output: ActionOutput,
    saved: Result<(), ActionFailure>,
) {
    let mut log = log.lock_recover();
    if let Some(entry) = log.entries.get_mut(&invocation) {
        entry.saving = false;
        entry.update = match saved {
            Ok(()) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => InvocationUpdate::Failed { invocation, error },
        };
    }
    log.moved();
}

/// Why an action whose change the host has not confirmed saving within
/// [`SAVE_WATCH`] failed. The change may still be saved (it may wait for its
/// record's keys), so asking again could make it twice: not retryable, the
/// caller reads the record first. With what the session says it keeps and
/// waits for (`kept`), where it said.
fn unconfirmed_failure(kept: Option<&str>) -> ActionFailure {
    let mut message = "the host has not confirmed saving the change within a minute; it may still \
                       be saved, so read the record before making the change again"
        .to_owned();
    if let Some(kept) = kept {
        message.push_str(": the session keeps ");
        message.push_str(kept);
    }
    ActionFailure {
        code: "save_unconfirmed".into(),
        message,
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
