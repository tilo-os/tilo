//! A write whose part has not arrived from the workspace: the service asks
//! for it, hands the app the parts its chains still want, and applies the
//! write again, bounded — so whoever asked hears what the write made. Only a
//! part that does not arrive in time leaves the refusal to its caller.
//!
//! While the part is on its way the write waits off the workspace's thread,
//! as a request waiting for its workspace's join or keys does: every request
//! of a workspace is served on one thread, and a write that held it for the
//! whole wait held every other request of the workspace behind it, a listing
//! as much as the person's next click. The thread looks again at the write
//! when the workspace changes and after a pause that grows while the part
//! stays away, and serves everything else in between.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tilo_actions::ActionOutput;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use super::{Job, Needs, Worker};

/// How long a write waits for the part it changes to arrive from the
/// workspace before its caller is told it has not, counted from when it was
/// asked: inside the wait the API's own caller makes, so it hears either what
/// the write made or why not.
pub(super) const PART_WAIT: Duration = Duration::from_secs(30);
/// The first pause before a write waiting for its part is applied again:
/// short, as a part usually arrives at once.
const PART_LOOK: Duration = Duration::from_millis(10);
/// The longest pause between those tries, as a part stays away: a part that arrives, or the
/// word that the workspace holds none of it, wakes the write at once, so the pause only
/// bounds what nothing announced. Every try takes the workspace's thread, and a shorter
/// one held the workspace's other requests behind tries that found nothing new.
const PART_LOOK_MAX: Duration = Duration::from_secs(1);

/// One operation to apply to what is mounted here, as the request named it.
pub(super) struct Invoked<'a> {
    /// The request it is part of: applied again, it takes the answer of the operation it
    /// handed off the thread ([`super::call_wait`]).
    pub(super) invocation: tilo_actions::InvocationId,
    pub(super) address: &'a tilo_actions::ActionAddress,
    pub(super) arguments: &'a serde_json::Map<String, serde_json::Value>,
    pub(super) context: Option<&'a tilo_actions::ActionContext>,
    pub(super) appearance: Option<&'a tilo_actions::Appearance>,
    pub(super) confirmed: bool,
}

/// How a write waited for its part so far.
#[derive(Clone, Copy)]
pub(super) struct Tried {
    /// When it was asked: its wait is counted from then ([`PART_WAIT`]).
    queued: Instant,
    /// The pause before it is applied again.
    look: Duration,
    /// Whether its person confirmed it when it was first applied: the answer
    /// is taken once, so a write applied again goes on as confirmed as it was.
    confirmed: Option<bool>,
}

/// A write held off the thread while its part arrives.
struct Awaiting {
    workspace_id: WorkspaceId,
    tried: Tried,
    /// When it is applied again at the latest.
    at: Instant,
    /// How far its workspace had moved when it was held ([`Worker::moved`]): a change of the
    /// workspace that landed nothing and answered nothing (a person arriving, a listing)
    /// brings no part, and the write is not applied again for it, which held the next
    /// request behind it every time.
    moved: u64,
    job: Job,
}

/// The writes of a thread's workspaces that wait for a part to arrive, and how
/// the request it serves now waited so far.
#[derive(Default)]
pub(super) struct PartWaits {
    awaiting: VecDeque<Awaiting>,
    /// The request being served: when it was asked, and how it waited for a
    /// part before.
    serving: Option<Tried>,
    /// The request being served waits for its part: it is held, not answered.
    held: Option<Tried>,
    /// How the write about to be applied again waited so far.
    resumed: Option<Tried>,
    /// Whether the request being served may wait off this thread for the parts of a chain
    /// still on their way ([`Worker::hold_chains`]): a write and a listing may.
    holdable: bool,
    /// The request being served found parts of a chain on their way and did not wait for
    /// them here: it is held until they arrive ([`Worker::hold_for_chains`]).
    chains_owed: bool,
    /// Whether the request being served is applied again after waiting off this thread.
    again: bool,
}

/// What a request does about parts of a chain still on their way ([`Worker::hold_chains`]).
pub(super) enum ChainWait {
    /// Waits for them here, as a request that cannot be held does.
    Here,
    /// Is held off this thread until they arrive.
    Held,
    /// Waited for them long enough: goes on without them.
    GaveUp,
}

impl PartWaits {
    /// Start serving the request asked at `queued`: a write applied again
    /// once its part arrived goes on with how it waited so far.
    /// A write or listing (`holdable`) waits off the thread for the parts of a chain too.
    pub(super) fn begin(&mut self, queued: Instant, holdable: bool) {
        self.holdable = holdable;
        self.chains_owed = false;
        self.again = self.resumed.is_some();
        self.serving = Some(self.resumed.take().unwrap_or(Tried {
            queued,
            look: PART_LOOK,
            confirmed: None,
        }));
        self.held = None;
    }

    /// Whether the request being served is applied again after waiting off this thread.
    pub(super) const fn again(&self) -> bool {
        self.again
    }

    /// What the request being served does about parts of a chain still on their way, waited
    /// for `wait` from when it was asked: held off this thread within it, when it may be.
    pub(super) fn chain_wait(&mut self, wait: Duration) -> ChainWait {
        let Some(tried) = self.serving.filter(|_| self.holdable) else {
            return ChainWait::Here;
        };
        if tried.queued.elapsed() >= wait {
            return ChainWait::GaveUp;
        }
        self.chains_owed = true;
        ChainWait::Held
    }

    /// Whether the request being served may wait off this thread, for `wait` from when it
    /// was asked: a write or a listing within it.
    pub(super) fn may_wait(&self, wait: Duration) -> bool {
        self.serving
            .filter(|_| self.holdable)
            .is_some_and(|tried| tried.queued.elapsed() < wait)
    }

    /// Whether the request being served waits for its part: its caller is not
    /// answered now, and what it did is not recorded as its outcome.
    pub(super) const fn held(&self) -> bool {
        self.held.is_some()
    }

    /// The request being served answers now whatever it waits for.
    pub(super) const fn release(&mut self) {
        self.held = None;
    }

    /// Whether the person confirmed the write being served, as `first` reads
    /// their answer: a write applied again once its part arrived goes on as
    /// confirmed as it was the first time, as the answer is taken only once.
    pub(super) fn confirmed_or(&self, first: impl FnOnce() -> bool) -> bool {
        self.serving
            .and_then(|tried| tried.confirmed)
            .unwrap_or_else(first)
    }

    /// How the request being served waited so far, with `confirmed`, its person's answer, kept
    /// for when it is applied again ([`super::call_wait`]).
    pub(super) fn serving_confirmed(&self, confirmed: bool) -> Option<Tried> {
        self.serving.map(|tried| Tried {
            confirmed: tried.confirmed.or(Some(confirmed)),
            ..tried
        })
    }

    /// How the request about to be applied again waited so far, taken from it: it waits once
    /// more, for something else ([`super::call_wait`]).
    pub(super) const fn take_resumed(&mut self) -> Option<Tried> {
        self.resumed.take()
    }

    /// The request about to be applied again waited as `tried` so far.
    pub(super) const fn resume_with(&mut self, tried: Option<Tried>) {
        self.resumed = tried;
    }

    /// Whether a write of `workspace_id` waits for its part.
    pub(super) fn any_in(&self, workspace_id: WorkspaceId) -> bool {
        self.awaiting
            .iter()
            .any(|awaiting| awaiting.workspace_id == workspace_id)
    }

    /// Whether no write waits for its part.
    pub(super) fn is_empty(&self) -> bool {
        self.awaiting.is_empty()
    }

    /// When the next waiting write is applied again.
    pub(super) fn until(&self) -> Option<Instant> {
        self.awaiting.iter().map(|awaiting| awaiting.at).min()
    }
}

impl Worker {
    /// Apply `invoked` to what is mounted at `holder`. What it changes not
    /// arrived from the workspace is asked for, the parts its chains still want
    /// are handed to the app, and it is applied again; a part still on its way
    /// holds the request off this thread until it arrives ([`Self::hold_for_part`]),
    /// bounded ([`PART_WAIT`]) — so a write into a chain's part answers what it
    /// made rather than nothing. Only a part that does not arrive in that time
    /// leaves the operation's refusal to whoever asked.
    pub(super) fn invoke_on(
        &mut self,
        workspace_id: WorkspaceId,
        holder: &DocumentRoute,
        needs: Needs<'_>,
        invoked: &Invoked<'_>,
    ) -> Result<ActionOutput, tilo_actions::api::ApiError> {
        let error = match self.invoke_mounted(workspace_id, holder, invoked) {
            Ok(output) => return Ok(output),
            Err(error) => error,
        };
        if !arriving(&error) {
            return Err(error);
        }
        let state = self.refresh(workspace_id, needs).map_err(|refused| {
            tilo_actions::api::ApiError::Unavailable {
                code: refused.code().to_owned(),
                reason: refused.to_string(),
            }
        })?;
        self.hold_chains(&super::Writer::Mounted(holder.clone()), holder, &state);
        if self.hold_for_chains() {
            return Err(not_ready());
        }
        let error = match self.invoke_mounted(workspace_id, holder, invoked) {
            Ok(output) => return Ok(output),
            Err(error) => error,
        };
        if arriving(&error) {
            self.hold_for_part(Some(invoked.confirmed));
        }
        Err(error)
    }

    /// The request being served found parts of a chain it reads on their way and did not
    /// wait for them here ([`Worker::hold_chains`]): have it wait off this thread for them,
    /// as for a part it changes. Whether it waits: nothing more of it is done now.
    pub(super) fn hold_for_chains(&mut self) -> bool {
        std::mem::take(&mut self.part_waits.chains_owed) && self.hold_for_part(None)
    }

    /// The request being served found its part not arrived: have it wait off
    /// this thread for the part, unless it waited [`PART_WAIT`] already, which
    /// leaves the refusal to its caller. Whether it waits. `confirmed` is whether its person
    /// confirmed it, when it was asked of them yet.
    pub(super) fn hold_for_part(&mut self, confirmed: Option<bool>) -> bool {
        let now = Instant::now();
        let tried = self.part_waits.serving.unwrap_or(Tried {
            queued: now,
            look: PART_LOOK,
            confirmed: None,
        });
        if tried
            .queued
            .checked_add(PART_WAIT)
            .is_none_or(|until| now >= until)
        {
            return false;
        }
        self.part_waits.held = Some(Tried {
            confirmed: tried.confirmed.or(confirmed),
            ..tried
        });
        true
    }

    /// Hold `job`, the request just served, off this thread until its part
    /// arrives: applied again on the workspace's next change, or after a pause
    /// that grows while the part stays away.
    pub(super) fn await_part(&mut self, workspace_id: WorkspaceId, job: Job) {
        let Some(tried) = self.part_waits.held.take() else {
            return;
        };
        let at = Instant::now()
            .checked_add(tried.look)
            .unwrap_or_else(Instant::now);
        let moved = self.moved(workspace_id);
        self.part_waits.awaiting.push_back(Awaiting {
            workspace_id,
            moved,
            tried: Tried {
                look: tried.look.saturating_mul(2).min(PART_LOOK_MAX),
                ..tried
            },
            at,
            job,
        });
        // The part arriving changes the workspace, which wakes this thread.
        self.awaiting(workspace_id, true);
    }

    /// Apply again the writes waiting for their part whose pause is over, and
    /// every one of `changed`, the workspace that just changed, once something
    /// landed in it or the workspace answered for a document since they were held.
    pub(super) fn try_parts_again(&mut self, changed: Option<WorkspaceId>) {
        let now = Instant::now();
        let landed = changed.map(|workspace_id| (workspace_id, self.moved(workspace_id)));
        let (due, waiting): (VecDeque<_>, VecDeque<_>) =
            std::mem::take(&mut self.part_waits.awaiting)
                .into_iter()
                .partition(|awaiting| {
                    awaiting.at <= now
                        || landed.is_some_and(|(workspace_id, moved)| {
                            awaiting.workspace_id == workspace_id && awaiting.moved != moved
                        })
                });
        self.part_waits.awaiting = waiting;
        for Awaiting {
            workspace_id,
            tried,
            job,
            ..
        } in due
        {
            self.part_waits.resumed = Some(tried);
            self.timed(tried.queued, job);
            self.stop_waiting_if_idle(workspace_id);
        }
    }

    /// How far `workspace_id` moved as this service holds it: the writes that landed in it,
    /// and the documents the workspace answered for.
    fn moved(&self, workspace_id: WorkspaceId) -> u64 {
        self.workspaces.get(&workspace_id).map_or(0, |workspace| {
            let landings = workspace
                .documents
                .as_ref()
                .map_or(0, crate::workspace_documents::WorkspaceDocuments::landings);
            let answered = u64::try_from(workspace.answered.len()).unwrap_or(u64::MAX);
            landings.wrapping_add(answered)
        })
    }

    /// [`Self::invoke_on`] once: `invoked` on what is mounted at `holder`.
    fn invoke_mounted(
        &mut self,
        workspace_id: WorkspaceId,
        holder: &DocumentRoute,
        invoked: &Invoked<'_>,
    ) -> Result<ActionOutput, tilo_actions::api::ApiError> {
        let app = self
            .mounted
            .get(holder)
            .map(|mounted| std::sync::Arc::clone(&mounted.app))
            .ok_or_else(|| tilo_actions::api::ApiError::Unknown(invoked.address.clone()))?;
        self.invoke_handed(
            (workspace_id, holder),
            (&app, invoked.invocation),
            invoked.confirmed,
            || crate::worker::Operation {
                context: invoked.context.cloned(),
                appearance: invoked.appearance.cloned(),
                address: invoked.address.clone(),
                arguments: invoked.arguments.clone(),
                confirm: invoked.confirmed,
            },
        )
    }
}

/// The refusal of a request held while what it reads arrives: its caller hears it only if
/// it is not held after all.
pub(super) fn not_ready() -> tilo_actions::api::ApiError {
    tilo_actions::api::ApiError::Unavailable {
        code: tilo_actions::api::NOT_READY.to_owned(),
        reason: tilo_actions::api::NOT_READY_REASON.to_owned(),
    }
}

/// Whether `error` says what the operation changes has not arrived yet.
pub(super) fn arriving(error: &tilo_actions::api::ApiError) -> bool {
    matches!(
        error,
        tilo_actions::api::ApiError::Unavailable { code, .. }
            if code == tilo_actions::api::NOT_READY
    )
}
