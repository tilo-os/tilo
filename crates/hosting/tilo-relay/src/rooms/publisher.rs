//! A room's governance, admitted to its workspace's host order off the room's
//! lock.
//!
//! Every decision ends in [`super::Rooms::settle`], which retains the
//! governance changes the decision published. Admitting a change to the
//! workspace's ordered owner waits for room in the owner's queue, and in a
//! crowd the owner's queue is full: a settle waiting there under the room's
//! lock held every pre-admission, challenge and redemption of the room
//! behind it for seconds. The publisher takes those admissions off the
//! lock. A settle reserves each change's place with
//! [`RoomRecords::reserve`] (so whoever reads the room's retention waits for
//! it from then on) and hands the changes over in the room's publication
//! order; one task per room admits them in that order and then waits for
//! them to be durable.
//!
//! A change that cannot be admitted drops it and every change handed over
//! after it, so none is admitted out of order, and leaves its position for
//! the next settle ([`RoomPublisher::reconcile`]), which republishes from
//! there as a settle that failed did before.
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{mpsc, watch};

use crate::room_records::{HostCommit, Reservation, RoomRecords};
use tilo_lock::MutexRecover as _;

/// One change to admit: its position in the room's publication (none for a
/// channel change, which is not republished), the commit, and its
/// reserved place.
pub(super) struct Publication {
    pub(super) position: Option<usize>,
    pub(super) commit: HostCommit,
    reservation: Reservation,
}

impl Publication {
    pub(super) fn new(
        records: &RoomRecords,
        workspace_id: u64,
        position: Option<usize>,
        commit: HostCommit,
    ) -> Self {
        Self {
            position,
            commit,
            reservation: records.reserve(workspace_id),
        }
    }
}

struct Batch {
    number: u64,
    epoch: u64,
    publications: Vec<Publication>,
}

#[derive(Default)]
struct Progress {
    /// Batches handed over so far.
    pushed: u64,
    /// Bumped by each failed admission: a batch handed over under an
    /// earlier epoch is dropped, not admitted.
    epoch: u64,
    /// The first publication position a failed admission left unretained.
    failed: Option<usize>,
    /// Publication positions handed over and not durable yet, each with how
    /// many changes at it are pending: the room file must not claim them
    /// published, or a hard stop before they are durable loses them for
    /// good, since the next start republishes only from `published` on.
    pending: std::collections::BTreeMap<usize, usize>,
}

impl Progress {
    fn hand_over(&mut self, positions: impl IntoIterator<Item = Option<usize>>) {
        for position in positions.into_iter().flatten() {
            let pending = self.pending.entry(position).or_default();
            *pending = pending.saturating_add(1);
        }
    }

    fn settled(&mut self, positions: impl IntoIterator<Item = Option<usize>>) {
        for position in positions.into_iter().flatten() {
            if let std::collections::btree_map::Entry::Occupied(mut entry) =
                self.pending.entry(position)
            {
                let pending = entry.get_mut();
                *pending = pending.saturating_sub(1);
                if *pending == 0 {
                    entry.remove();
                }
            }
        }
    }
}

struct Shared {
    progress: Mutex<Progress>,
    /// The number of the last batch the task finished with.
    finished: watch::Sender<u64>,
}

impl Shared {
    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock_recover()
    }
}

/// The ordered admission of one room's governance changes.
pub(super) struct RoomPublisher {
    queue: OnceLock<mpsc::UnboundedSender<Batch>>,
    shared: Arc<Shared>,
}

impl Default for RoomPublisher {
    fn default() -> Self {
        Self {
            queue: OnceLock::new(),
            shared: Arc::new(Shared {
                progress: Mutex::new(Progress::default()),
                finished: watch::channel(0).0,
            }),
        }
    }
}

/// The epoch a settle hands its changes over under, from
/// [`RoomPublisher::reconcile`].
#[derive(Clone, Copy)]
pub(super) struct Epoch(u64);

impl RoomPublisher {
    /// Move `published` back to the first position an admission failed to
    /// retain since the last call, and return the epoch to hand the next
    /// changes over under. A failure after this drops those changes too,
    /// and the next call moves `published` back over them.
    pub(super) fn reconcile(&self, published: &mut usize) -> Epoch {
        let mut progress = self.shared.progress();
        if let Some(failed) = progress.failed.take() {
            *published = (*published).min(failed);
        }
        Epoch(progress.epoch)
    }

    /// The first publication position not known to be retained: one a
    /// failed admission left unretained (until the next [`Self::reconcile`]
    /// takes it) or one handed over and not durable yet. A room record
    /// written meanwhile must not claim it published.
    pub(super) fn unretained_from(&self) -> Option<usize> {
        let progress = self.shared.progress();
        let pending = progress.pending.keys().next().copied();
        match (progress.failed, pending) {
            (Some(failed), Some(pending)) => Some(failed.min(pending)),
            (failed, pending) => failed.or(pending),
        }
    }

    /// Hand `publications` over to be admitted after every batch handed
    /// over before them, in their order. Returns at once.
    pub(super) fn push(
        &self,
        records: &RoomRecords,
        room: &str,
        workspace_id: u64,
        epoch: Epoch,
        publications: Vec<Publication>,
    ) {
        if publications.is_empty() {
            return;
        }
        let queue = self.queue.get_or_init(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(run(
                rx,
                records.clone(),
                room.to_owned(),
                workspace_id,
                Arc::clone(&self.shared),
            ));
            tx
        });
        // Numbered and queued in one step, so batches finish in number order.
        let mut progress = self.shared.progress();
        progress.hand_over(publications.iter().map(|p| p.position));
        progress.pushed = progress.pushed.saturating_add(1);
        let number = progress.pushed;
        let batch = Batch {
            number,
            epoch: epoch.0,
            publications,
        };
        if let Err(mpsc::error::SendError(batch)) = queue.send(batch) {
            // The task ended with the runtime: nothing was admitted.
            drop(progress);
            let positions = batch.publications.iter().map(|p| p.position);
            unretained(&self.shared, positions, true);
            self.shared.finished.send_replace(number);
        }
    }

    /// Wait until every change handed over so far is admitted to the
    /// host's order, or dropped. Whoever admits a change of the room's
    /// channels on its own waits for this first, under the steward, so a
    /// channel change a settle handed over keeps its place before theirs.
    pub(super) async fn flushed(&self) {
        let target = self.shared.progress().pushed;
        let mut finished = self.shared.finished.subscribe();
        // The sender lives in `shared`, which this borrow keeps alive.
        let _ = finished.wait_for(|done| *done >= target).await;
    }
}

/// Record that changes at `positions` were not admitted. `failed` is a new
/// failure, which drops every batch handed over before it; a batch dropped
/// for an earlier failure only adds its positions.
///
/// A channel change has no position: the steward moved on when it handed
/// the change over, so a dropped one is gone, as one whose own admission
/// failed always was. Say so.
fn unretained(shared: &Shared, positions: impl IntoIterator<Item = Option<usize>>, failed: bool) {
    let positions: Vec<Option<usize>> = positions.into_iter().collect();
    let mut channel_changes = 0usize;
    let first = positions
        .iter()
        .copied()
        .filter_map(|position| {
            if position.is_none() {
                channel_changes = channel_changes.saturating_add(1);
            }
            position
        })
        .min();
    if channel_changes != 0 {
        tracing::warn!(
            channel_changes,
            "channel governance dropped: a change of the room was not admitted"
        );
    }
    let mut progress = shared.progress();
    if failed {
        progress.epoch = progress.epoch.wrapping_add(1);
    }
    // Handed back: no longer waited on as pending.
    progress.settled(positions.iter().copied());
    if let Some(first) = first {
        progress.failed = Some(progress.failed.map_or(first, |known| known.min(first)));
    }
}

async fn run(
    mut queue: mpsc::UnboundedReceiver<Batch>,
    records: RoomRecords,
    room: String,
    workspace_id: u64,
    shared: Arc<Shared>,
) {
    while let Some(Batch {
        number,
        epoch,
        publications,
    }) = queue.recv().await
    {
        let mut admissions = Vec::with_capacity(publications.len());
        if shared.progress().epoch == epoch {
            let mut publications = publications.into_iter();
            while let Some(publication) = publications.next() {
                let position = publication.position;
                match records
                    .enqueue_reserved(workspace_id, publication.commit, publication.reservation)
                    .await
                {
                    Ok(admission) => admissions.push((position, admission)),
                    Err(error) => {
                        tracing::warn!(%room, %error, "room governance change was not admitted");
                        let rest = publications.map(|publication| publication.position);
                        unretained(&shared, std::iter::once(position).chain(rest), true);
                        break;
                    }
                }
            }
        } else {
            let positions = publications.iter().map(|publication| publication.position);
            unretained(&shared, positions, false);
        }
        shared.finished.send_replace(number);
        if !admissions.is_empty() {
            let room = room.clone();
            let shared = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut admissions = admissions.into_iter();
                while let Some((position, admission)) = admissions.next() {
                    // Settled before a reader of the room's retention sees the
                    // change retained, so a decision after that persists it as
                    // published.
                    let retained = admission
                        .durable_then(|outcome| {
                            if outcome.is_ok() {
                                shared.progress().settled([position]);
                            }
                            outcome.clone().map(drop)
                        })
                        .await;
                    if let Err(error) = retained {
                        tracing::warn!(%room, %error, "room governance change was not retained");
                        // Not retained: the next settle publishes it again,
                        // and what was admitted after it with it.
                        let rest = admissions.map(|(position, _)| position);
                        unretained(&shared, std::iter::once(position).chain(rest), false);
                        break;
                    }
                }
            });
        }
    }
}
