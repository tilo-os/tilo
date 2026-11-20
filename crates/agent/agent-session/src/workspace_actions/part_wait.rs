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
/// The longest pause between those tries, as a part stays away.
const PART_LOOK_MAX: Duration = Duration::from_millis(250);

/// One operation to apply to what is mounted here, as the request named it.
pub(super) struct Invoked<'a> {
    pub(super) address: &'a tilo_actions::ActionAddress,
    pub(super) arguments: &'a serde_json::Map<String, serde_json::Value>,
    pub(super) context: Option<&'a tilo_actions::ActionContext>,
    pub(super) appearance: Option<&'a tilo_actions::Appearance>,
    pub(super) confirmed: bool,
}

/// How a write waited for its part so far.
#[derive(Clone, Copy)]
struct Tried {
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
}

impl PartWaits {
    /// Start serving the request asked at `queued`: a write applied again
    /// once its part arrived goes on with how it waited so far.
    pub(super) fn begin(&mut self, queued: Instant) {
        self.serving = Some(self.resumed.take().unwrap_or(Tried {
            queued,
            look: PART_LOOK,
            confirmed: None,
        }));
        self.held = None;
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
        let error = match self.invoke_mounted(holder, invoked) {
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
        let error = match self.invoke_mounted(holder, invoked) {
            Ok(output) => return Ok(output),
            Err(error) => error,
        };
        if arriving(&error) {
            self.hold_for_part(invoked.confirmed);
        }
        Err(error)
    }

    /// The request being served found its part not arrived: have it wait off
    /// this thread for the part, unless it waited [`PART_WAIT`] already, which
    /// leaves the refusal to its caller. Whether it waits.
    pub(super) fn hold_for_part(&mut self, confirmed: bool) -> bool {
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
            confirmed: tried.confirmed.or(Some(confirmed)),
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
        self.part_waits.awaiting.push_back(Awaiting {
            workspace_id,
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
    /// every one of `changed`, the workspace that just changed.
    pub(super) fn try_parts_again(&mut self, changed: Option<WorkspaceId>) {
        let now = Instant::now();
        let (due, waiting): (VecDeque<_>, VecDeque<_>) =
            std::mem::take(&mut self.part_waits.awaiting)
                .into_iter()
                .partition(|awaiting| awaiting.at <= now || Some(awaiting.workspace_id) == changed);
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

    /// [`Self::invoke_on`] once: `invoked` on what is mounted at `holder`.
    fn invoke_mounted(
        &mut self,
        holder: &DocumentRoute,
        invoked: &Invoked<'_>,
    ) -> Result<ActionOutput, tilo_actions::api::ApiError> {
        let mounted = self
            .mounted
            .get_mut(holder)
            .ok_or_else(|| tilo_actions::api::ApiError::Unknown(invoked.address.clone()))?;
        mounted.app.invoke_in(
            invoked.context,
            invoked.appearance,
            invoked.address,
            invoked.arguments,
            invoked.confirmed,
        )
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
