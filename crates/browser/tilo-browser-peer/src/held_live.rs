//! Live frames waiting for their record's channel keys.
//!
//! A live frame travels only sealed with the keys of the record it names. A
//! device learns a record's keys a moment after the record itself: an agent
//! that just created a record, or a page that just saw it arrive. A frame in
//! that moment waits here instead of being lost (an agent says what it is
//! doing once, as it does it) and goes out, or is read, once the keys are
//! installed. Waiting is bounded: the oldest frames give way.

use std::collections::{BTreeMap, VecDeque};

use tilo_relay_protocol::live::LiveRecord;

use crate::session::LiveCompletion;

/// Frames kept per record or channel.
const PER_KEY: usize = 16;
/// Records and channels with waiting frames.
const KEYS: usize = 64;

/// One live frame waiting for keys.
pub(crate) enum Waiting {
    /// A canonical frame to seal and send on the session.
    Send { workspace_id: u64, frame: Vec<u8> },
    /// A canonical frame to seal for another carrier.
    Seal {
        workspace_id: u64,
        frame: Vec<u8>,
        done: LiveCompletion,
    },
    /// A sealed frame the host relayed.
    Receive {
        workspace_id: u64,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// A sealed frame another carrier delivered.
    Open {
        workspace_id: u64,
        frame: Vec<u8>,
        done: LiveCompletion,
    },
}

impl Waiting {
    /// Give up on the frame: whoever waits on it learns it is gone.
    fn drop_frame(self) {
        match self {
            Self::Seal { done, .. } | Self::Open { done, .. } => done(None),
            Self::Send { .. } | Self::Receive { .. } => {}
        }
    }
}

/// Bounded frames per key, the oldest giving way.
struct Bounded<K: Ord> {
    waiting: BTreeMap<K, VecDeque<Waiting>>,
}

impl<K: Ord + Clone> Bounded<K> {
    const fn new() -> Self {
        Self {
            waiting: BTreeMap::new(),
        }
    }

    fn hold(&mut self, key: K, frame: Waiting) {
        if !self.waiting.contains_key(&key)
            && self.waiting.len() >= KEYS
            && let Some((_, evicted)) = self.waiting.pop_first()
        {
            evicted.into_iter().for_each(Waiting::drop_frame);
        }
        let queue = self.waiting.entry(key).or_default();
        if queue.len() >= PER_KEY
            && let Some(oldest) = queue.pop_front()
        {
            oldest.drop_frame();
        }
        queue.push_back(frame);
    }

    fn release(&mut self, key: &K) -> VecDeque<Waiting> {
        self.waiting.remove(key).unwrap_or_default()
    }
}

/// Live frames waiting for keys: outbound by the record they name, inbound
/// by the channel that sealed them.
pub(crate) struct HeldLive {
    outbound: Bounded<(u64, LiveRecord)>,
    inbound: Bounded<[u8; 16]>,
}

impl HeldLive {
    pub(crate) const fn new() -> Self {
        Self {
            outbound: Bounded::new(),
            inbound: Bounded::new(),
        }
    }

    /// Keep an outbound frame of `workspace_id` until `record`'s keys arrive.
    pub(crate) fn hold_outbound(&mut self, workspace_id: u64, record: LiveRecord, frame: Waiting) {
        self.outbound.hold((workspace_id, record), frame);
    }

    /// Keep an inbound frame until `channel`'s keys arrive.
    pub(crate) fn hold_inbound(&mut self, channel: [u8; 16], frame: Waiting) {
        self.inbound.hold(channel, frame);
    }

    /// Everything that waited for the keys of `record` of `workspace_id`,
    /// whose channel is `channel`: outbound frames first, in order.
    pub(crate) fn release(
        &mut self,
        workspace_id: u64,
        record: LiveRecord,
        channel: [u8; 16],
    ) -> impl Iterator<Item = Waiting> {
        self.outbound
            .release(&(workspace_id, record))
            .into_iter()
            .chain(self.inbound.release(&channel))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn record(name: &str) -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: name.into(),
        }
    }

    #[test]
    fn frames_wait_for_their_keys_within_bounds() {
        let mut held = HeldLive::new();
        let dropped = Arc::new(Mutex::new(0));
        for seq in 0..=PER_KEY {
            let dropped = dropped.clone();
            held.hold_outbound(
                1,
                record("r"),
                Waiting::Seal {
                    workspace_id: 1,
                    frame: vec![u8::try_from(seq).unwrap()],
                    done: Box::new(move |sealed| {
                        assert!(sealed.is_none());
                        *dropped.lock().unwrap() += 1;
                    }),
                },
            );
        }
        assert_eq!(*dropped.lock().unwrap(), 1, "the oldest gave way");
        held.hold_inbound(
            [7; 16],
            Waiting::Receive {
                workspace_id: 1,
                from: [1; 32],
                frame: vec![9],
            },
        );
        let released: Vec<_> = held.release(1, record("r"), [7; 16]).collect();
        assert_eq!(released.len(), PER_KEY + 1);
        assert!(matches!(released.last(), Some(Waiting::Receive { .. })));
        assert!(matches!(
            released.first(),
            Some(Waiting::Seal { frame, .. }) if frame == &[1]
        ));
        assert_eq!(held.release(1, record("r"), [7; 16]).count(), 0);
        for frame in released {
            frame.drop_frame();
        }
    }
}
