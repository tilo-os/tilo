//! A linked member's write, carried to the relay's sessions until it lands.
//!
//! A page reads a workspace through a relay session: what it is sent of a
//! record is what the relay retains of it, and a record the relay sends
//! nothing of is one the page's app makes anew. So a write a desktop made
//! over the workspace's link is retained by the relay in its author's name
//! ([`Workspaces::carry_to_sessions`]), and a write whose copy failed is
//! tried again, after a growing wait, until it is durable at the relay. Each
//! workspace's writes are carried one after another in the order the link
//! took them in: one that waits holds back those after it, never overtaken.
//! Until a write lands, a page that opens its record is sent it from the
//! link's history ([`Workspaces::link_frames_lacking`]).

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::mpsc;

use super::{WeakWorkspaces, Workspaces};
use crate::workspace_records::HostCommit;

/// The first wait before a failed copy is tried again.
const FIRST_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
/// The longest wait between two tries of a copy that keeps failing.
const LONGEST_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// One write on its way to the relay's sessions.
struct Carried {
    workspace: String,
    number: u64,
    author: [u8; 32],
    commit: HostCommit,
}

/// Each workspace's carrier: the queue its writes wait in.
#[derive(Default)]
pub(super) struct SessionCarriers {
    queues: Mutex<HashMap<u64, mpsc::UnboundedSender<Carried>>>,
    /// Copies a test has fail before they are tried, as a relay whose
    /// sessions' store refuses a write for a while.
    #[cfg(test)]
    pub(super) failing: std::sync::atomic::AtomicUsize,
}

impl Workspaces {
    /// Queue `commit`, `author`'s write of `workspace` (relay number
    /// `number`), for the relay's sessions, behind the workspace's writes
    /// still on their way.
    pub(super) fn carry_until_landed(
        &self,
        workspace: &str,
        number: u64,
        author: [u8; 32],
        commit: HostCommit,
    ) {
        let carried = Carried {
            workspace: workspace.to_owned(),
            number,
            author,
            commit,
        };
        let mut queues = self
            .inner
            .session_carriers
            .queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let carried = match queues.get(&number) {
            Some(queue) => match queue.send(carried) {
                Ok(()) => return,
                // Its carrier ended: a new one takes the write.
                Err(mpsc::error::SendError(carried)) => carried,
            },
            None => carried,
        };
        let (queue, writes) = mpsc::unbounded_channel();
        // The receiver was made a line above and is still held.
        let _ = queue.send(carried);
        queues.insert(number, queue);
        drop(queues);
        let workspaces = WeakWorkspaces(std::sync::Arc::downgrade(&self.inner));
        tokio::spawn(carry_each(workspaces, writes));
    }

    /// Retain `carried` at the relay and wait until it is durable there:
    /// `Err` with why it did not land.
    async fn land(&self, carried: &Carried) -> Result<(), String> {
        #[cfg(test)]
        if self
            .inner
            .session_carriers
            .failing
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |left| left.checked_sub(1),
            )
            .is_ok()
        {
            return Err("the relay's store refused the write (a test's)".into());
        }
        self.inner
            .host
            .records
            .carry(carried.number, carried.author, carried.commit.clone())
            .await?
            .durable()
            .await
            .map(drop)
    }
}

/// Carry each write `writes` brings until it lands, in order, waiting longer
/// after each failed try of the same write. Ends with the relay, or once the
/// queue is closed and empty.
async fn carry_each(workspaces: WeakWorkspaces, mut writes: mpsc::UnboundedReceiver<Carried>) {
    while let Some(carried) = writes.recv().await {
        let mut wait = FIRST_RETRY;
        let mut tries = 0_u32;
        loop {
            let Some(relay) = workspaces.upgrade() else {
                return;
            };
            match relay.land(&carried).await {
                Ok(()) => {
                    if tries > 0 {
                        tracing::info!(workspace = %carried.workspace, tries, "a linked member's write reached the relay's sessions after failing");
                    }
                    break;
                }
                Err(error) => {
                    tries = tries.saturating_add(1);
                    tracing::warn!(workspace = %carried.workspace, %error, tries, retry_in_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX), "a linked member's write did not reach the relay's sessions; it is tried again, and pages are sent it from the link meanwhile");
                }
            }
            drop(relay);
            tokio::time::sleep(wait).await;
            wait = wait.saturating_mul(2).min(LONGEST_RETRY);
        }
    }
}

impl Workspaces {
    /// The writes of the record on `channel` that `number`'s link keeps and
    /// `held`, what the relay retains of the record, lacks: writes whose copy
    /// to the relay has not landed yet. Each comes as the relay's frame of
    /// it, the one its copy becomes once it lands, in the link's order. A
    /// page that opens the record is sent these beside the relay's own, so
    /// it never hears the record is absent while the workspace's host keeps
    /// it. A write a sealed compaction `held` covers is no write it lacks:
    /// the relay folded it into that compaction and let it go.
    ///
    /// # Errors
    ///
    /// The link's history cannot be read.
    pub(crate) fn link_frames_lacking(
        &self,
        number: u64,
        channel: [u8; 32],
        held: &[crate::transport::Envelope],
    ) -> Result<Vec<crate::transport::Envelope>, String> {
        let answered = RelayHistory::of(held.iter().filter_map(relay_frame_write));
        let mut cursors = Vec::new();
        let mut lacking = Vec::new();
        loop {
            let replay = self
                .inner
                .store
                .replay(number, &cursors, tilo_peer_state::store::Budget::default())
                .map_err(|error| error.to_string())?;
            for message in &replay.frames {
                let Some((on, retention)) = neighbour_wire::retention(message, false) else {
                    continue;
                };
                if on != channel {
                    continue;
                }
                let (Ok(frame), Some(author)) = (
                    tilo_peer_state::store::Frame::of(message),
                    tilo_peer_state::store::author_of(message),
                ) else {
                    continue;
                };
                if answered.answers_for(&frame) {
                    continue;
                }
                let body = postcard::to_allocvec(message).map_err(|error| error.to_string())?;
                let payload = if retention == neighbour_wire::Retention::Checkpoint {
                    crate::coedit::CoeditPayload::Checkpoint { channel, body }
                } else {
                    crate::coedit::CoeditPayload::RetainedDelta { channel, body }
                };
                lacking.push(crate::transport::Envelope::Delta {
                    workspace_id: number,
                    author_device_public_key: author,
                    authored_at_ns: tilo_clock::unix_now_ns(),
                    delta: payload.encode().map_err(|error| error.to_string())?,
                    signature: [0; 64],
                });
            }
            tilo_peer_state::store::advance(&mut cursors, &replay.frames);
            if !replay.more || replay.frames.is_empty() {
                return Ok(lacking);
            }
        }
    }
}

/// The record writes the relay's history answers for: each write it retains, and each write a
/// sealed compaction it retains covers. A fold lets the writes it covers go, so a write the link
/// keeps that a retained compaction covers is no write the relay lacks: carrying it back would put
/// it after the compaction that replaced it, where no reader takes it in.
pub(super) struct RelayHistory {
    held: std::collections::HashSet<(tilo_peer_state::store::Lineage, u64)>,
    /// Per author on a record's channel, the highest sequence a retained compaction of that
    /// record covers.
    covered: std::collections::HashMap<tilo_peer_state::store::Lineage, u64>,
}

impl RelayHistory {
    /// What `writes`, the record writes the relay retains, answer for.
    pub(super) fn of(writes: impl IntoIterator<Item = neighbour_wire::NeighbourMsg>) -> Self {
        let mut history = Self {
            held: std::collections::HashSet::new(),
            covered: std::collections::HashMap::new(),
        };
        for message in writes {
            let Ok(frame) = tilo_failure::ignored!(
                "only sealed record writes are what the link keeps; anything else matches none",
                tilo_peer_state::store::Frame::of(&message)
            ) else {
                continue;
            };
            if let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = &message
                && envelope.kind == collab_types::OpKind::Compaction
                && let Ok(compaction) = tilo_failure::ignored!(
                    "a compaction that does not parse covers nothing, and the writes it names are carried",
                    collab_types::SealedCompaction::parse(envelope)
                )
            {
                for (author, floor) in compaction.covers {
                    let lineage = tilo_peer_state::store::Lineage {
                        channel: frame.lineage.channel,
                        author: *author.as_bytes(),
                    };
                    let at = history.covered.entry(lineage).or_default();
                    *at = (*at).max(floor);
                }
            }
            history.held.insert((frame.lineage, frame.seq));
        }
        history
    }

    /// Whether the relay's history answers for `frame`, a write the link keeps.
    pub(super) fn answers_for(&self, frame: &tilo_peer_state::store::Frame) -> bool {
        self.held.contains(&(frame.lineage, frame.seq))
            || self
                .covered
                .get(&frame.lineage)
                .is_some_and(|floor| frame.seq <= *floor)
    }
}

/// The record write a relay frame carries, when it carries one.
fn relay_frame_write(
    envelope: &crate::transport::Envelope,
) -> Option<neighbour_wire::NeighbourMsg> {
    let crate::transport::Envelope::Delta { delta, .. } = envelope else {
        return None;
    };
    let (crate::coedit::CoeditPayload::Checkpoint { body, .. }
    | crate::coedit::CoeditPayload::RetainedDelta { body, .. }) =
        crate::coedit::CoeditPayload::decode(delta)?;
    tilo_failure::ignored!(
        "a retained body that does not decode holds no write the link could match",
        postcard::from_bytes(&body).ok()
    )
}
