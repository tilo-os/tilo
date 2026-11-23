//! Pieces of record content a connection could not take when they came, kept
//! one queue per sender and served in turn.
//!
//! Content is routed point to point and never stored: a piece that does not
//! fit the recipient's outbound queue is dropped, and the device that wanted
//! it asks again. That is fine for one sender, but a plain first-in queue
//! lets one member's burst fill the space every other member's pieces need.
//! Held here instead, a sender's overflow waits behind at most one piece of
//! each other sender, and its own pieces spill once its queue is full, so a
//! busy sender slows only itself.
//!
//! [`ContentQueue::serve_waiting`] moves held pieces back into the
//! connection's queue as its writer drains it, and
//! [`ContentQueue::deliver_piece`] does so on every piece. Nothing here
//! awaits: the queue is a plain lock held for a moment.

use crate::transport::Envelope;
use std::collections::VecDeque;
use std::sync::Mutex;
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

/// Pieces one sender may have waiting: one answer to a content request
/// (`neighbour_wire::content::PIECES_PER_ANSWER`). Beyond it that sender's
/// pieces spill, and its device asks again for what it still lacks.
pub(crate) const PER_SENDER: usize = 64;
/// Pieces all senders together may have waiting for one connection: two of
/// these windows, so one sender's burst and another's are both carried
/// while the connection drains.
const ALL_SENDERS: usize = 2 * PER_SENDER;

/// The content pieces waiting for one connection's outbound queue.
#[derive(Default)]
pub struct ContentQueue {
    queues: Mutex<SenderQueues>,
}

#[derive(Default)]
struct SenderQueues {
    /// Each sender's waiting pieces, in the order the senders were first
    /// seen: served front first, so every sender takes its turn.
    senders: VecDeque<SenderPieces>,
    /// Pieces waiting over all senders.
    pieces: usize,
}

struct SenderPieces {
    from: [u8; 32],
    queue: VecDeque<Envelope>,
}

impl ContentQueue {
    /// Hand `envelope`, which `from` sent, to the connection behind `tx`, or
    /// hold it until that queue has room.
    pub fn deliver_piece(&self, from: [u8; 32], envelope: Envelope, tx: &mpsc::Sender<Envelope>) {
        self.keep_piece(from, envelope);
        self.serve_waiting(tx);
    }

    /// Move held pieces into `tx` in turn while it has room.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the queue is served under one lock from the first sender to the last"
    )]
    pub fn serve_waiting(&self, tx: &mpsc::Sender<Envelope>) {
        let mut queues = self.queues.lock_recover();
        while let Some(mut sender) = queues.senders.pop_front() {
            let Some(envelope) = sender.queue.pop_front() else {
                continue;
            };
            match tx.try_send(envelope) {
                Ok(()) => {
                    queues.pieces = queues.pieces.saturating_sub(1);
                    if !sender.queue.is_empty() {
                        queues.senders.push_back(sender);
                    }
                }
                // Backpressure, not absence: the piece goes back at the front
                // of its sender's queue, and the senders behind it wait for
                // the next turn, as this one does.
                Err(mpsc::error::TrySendError::Full(envelope)) => {
                    sender.queue.push_front(envelope);
                    queues.senders.push_front(sender);
                    break;
                }
                // The connection is gone: what waited for it goes too.
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    queues.senders.clear();
                    queues.pieces = 0;
                    break;
                }
            }
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "a sender's queue is found and capped under one lock"
    )]
    fn keep_piece(&self, from: [u8; 32], envelope: Envelope) {
        let mut queues = self.queues.lock_recover();
        let index = queues
            .senders
            .iter()
            .position(|sender| sender.from == from)
            .unwrap_or_else(|| {
                queues.senders.push_back(SenderPieces {
                    from,
                    queue: VecDeque::new(),
                });
                queues.senders.len().saturating_sub(1)
            });
        let dropped = queues.senders.get_mut(index).is_some_and(|sender| {
            let dropped = sender.queue.len() >= PER_SENDER;
            if dropped {
                sender.queue.pop_front();
            }
            sender.queue.push_back(envelope);
            dropped
        });
        queues.pieces = queues
            .pieces
            .saturating_add(1)
            .saturating_sub(usize::from(dropped));
        // Past what the connection may be held for, the sender with the most
        // waiting sheds a piece: whoever is behind comes in, never the other
        // way round.
        while queues.pieces > ALL_SENDERS {
            let longest = queues
                .senders
                .iter()
                .enumerate()
                .max_by_key(|(_, sender)| sender.queue.len())
                .map(|(index, _)| index);
            let Some(longest) = longest else {
                break;
            };
            let Some(sender) = queues.senders.get_mut(longest) else {
                break;
            };
            if sender.queue.pop_front().is_none() {
                break;
            }
            let emptied = sender.queue.is_empty();
            queues.pieces = queues.pieces.saturating_sub(1);
            if emptied {
                queues.senders.remove(longest);
            }
        }
    }
}
