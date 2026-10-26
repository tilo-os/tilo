//! Byte custody for immutable native reads. Reservations precede capture work:
//! each capture reserves the most a native save may produce before it runs,
//! and keeps only what the save produced for as long as the capture lives.
//!
//! The budget is a queue, not a gate: a read that finds it held waits its
//! turn ([`Budget::wait`]), served in order as reservations are released or
//! shrink. Running out of room is load, never a refusal.
use runtime_collab::committed_document::{
    CHANGES_FIRST_BYTES, CHUNK_BYTES, MAX_CHANGES_BASE, MAX_DOCUMENT_BYTES,
};
use std::sync::Arc;
use tokio::sync::Semaphore;
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// The actual bounded native save remains owned by its provider.
pub trait DocumentBytes: Send + Sync {
    fn bytes(&self) -> &[u8];
}
/// The bytes every capture of the broker holds, as permits of one fair
/// semaphore: a waiting read is served before any later one, and a release
/// goes to the oldest waiting read first.
pub(crate) struct Budget {
    total: usize,
    permits: Arc<Semaphore>,
}
impl Default for Budget {
    fn default() -> Self {
        Self::with_total(TOTAL_BYTES)
    }
}
/// Permits taken out of the budget's semaphore, given back as the capture
/// shrinks and when it ends.
pub(in crate::collab_broker_capnp) struct Reservation {
    permits: Arc<Semaphore>,
    bytes: usize,
}
impl Budget {
    pub(in crate::collab_broker_capnp) fn with_total(total: usize) -> Self {
        let total = total.clamp(1, Semaphore::MAX_PERMITS);
        Self {
            total,
            permits: Arc::new(Semaphore::new(total)),
        }
    }
    /// What a capture reserves before its save runs: the most a save may
    /// produce, or the whole budget when that is more. One capture larger
    /// than the budget is then served once nothing else is held, rather than
    /// never.
    fn admitted(&self) -> u32 {
        u32::try_from(MAX_DOCUMENT_BYTES.min(self.total)).unwrap_or(u32::MAX)
    }
    /// A reservation now, if the budget has room and no read is waiting for
    /// it: a read never overtakes one that waits.
    pub(in crate::collab_broker_capnp) fn try_reserve(self: &Arc<Self>) -> Option<Reservation> {
        let admitted = self.admitted();
        self.permits
            .try_acquire_many(admitted)
            .ok()
            .map(|permit| Reservation::of(&self.permits, permit, admitted))
    }
    /// A reservation once this read's turn comes: after every read that
    /// waited before it, as soon as releases leave room. `None` only if the
    /// budget were closed, which it never is.
    pub(in crate::collab_broker_capnp) fn wait(
        self: &Arc<Self>,
    ) -> impl std::future::Future<Output = Option<Reservation>> + 'static {
        let permits = Arc::clone(&self.permits);
        let admitted = self.admitted();
        async move {
            let permit = permits.acquire_many(admitted).await.ok()?;
            Some(Reservation::of(&permits, permit, admitted))
        }
    }
    /// The bytes held now, by every reservation.
    #[cfg(test)]
    pub(in crate::collab_broker_capnp) fn used(&self) -> usize {
        self.total - self.permits.available_permits()
    }
}
impl Reservation {
    fn of(permits: &Arc<Semaphore>, permit: tokio::sync::SemaphorePermit<'_>, bytes: u32) -> Self {
        permit.forget();
        Self {
            permits: Arc::clone(permits),
            bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
        }
    }
    /// Keep only `bytes` of this reservation, once the capture it was made
    /// for holds that many: a capture waiting to be read holds its body, not
    /// the most any body could be. What is released goes to the oldest
    /// waiting read.
    fn keep(&mut self, bytes: usize) {
        let released = self.bytes.saturating_sub(bytes);
        self.permits.add_permits(released);
        self.bytes = self.bytes.saturating_sub(released);
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.permits.add_permits(self.bytes);
    }
}
pub(super) struct Capture {
    body: Box<dyn DocumentBytes>,
    // Drop body (and its native charges) before releasing this quota.
    _reservation: Reservation,
    pub(super) id: u64,
    pub(super) revision: [u8; 32],
    /// The heads `revision` names, when the owner said; what a later read
    /// on this channel may hand out the changes since.
    heads: Vec<[u8; 32]>,
    /// Set for changes rather than a whole document: the heads they apply
    /// to. The first piece is then a `Changes` reply.
    base: Option<Vec<[u8; 32]>>,
    next: usize,
    token: [u8; 16],
    deadline: std::time::Instant,
    expired: bool,
}
impl Capture {
    pub(super) fn new(
        body: Box<dyn DocumentBytes>,
        mut reservation: Reservation,
        id: u64,
        revision: [u8; 32],
    ) -> Result<Self, String> {
        let len = body.bytes().len();
        if len == 0 || len > MAX_DOCUMENT_BYTES || id == 0 {
            return Err("invalid native document capture".into());
        }
        reservation.keep(len);
        let now = std::time::Instant::now();
        Ok(Self {
            body,
            _reservation: reservation,
            id,
            revision,
            heads: Vec::new(),
            base: None,
            next: 0,
            token: [0; 16],
            deadline: now
                .checked_add(std::time::Duration::from_secs(5))
                .unwrap_or(now),
            expired: false,
        })
    }
    /// The heads `revision` names, for a later changes read on this channel.
    pub(super) fn naming(mut self, heads: Vec<[u8; 32]>) -> Self {
        self.heads = heads;
        self
    }
    /// These bytes are the changes since the document with heads `base`.
    pub(super) fn changes_since(mut self, base: Vec<[u8; 32]>) -> Result<Self, String> {
        if base.is_empty() || base.len() > MAX_CHANGES_BASE {
            return Err("invalid native document changes base".into());
        }
        self.base = Some(base);
        Ok(self)
    }
    /// The revision, with its heads, this capture hands out once its last
    /// piece is acknowledged; `None` when the owner did not name its heads
    /// or they are too many to start changes from.
    pub(super) fn delivers(&self) -> Option<super::HeldRevision> {
        (self.finished() && !self.heads.is_empty() && self.heads.len() <= MAX_CHANGES_BASE).then(
            || super::HeldRevision {
                revision: self.revision,
                heads: self.heads.clone(),
            },
        )
    }
    pub(super) fn chunk(
        &mut self,
        offset: u32,
        token: [u8; 16],
    ) -> Result<runtime_collab::committed_document::Reply, String> {
        self.check_deadline(std::time::Instant::now())?;
        if token != self.token {
            return Err("committed document continuation token mismatch".into());
        }
        if offset as usize != self.next {
            return Err("committed document cursor mismatch".into());
        }
        let total = self.body.bytes().len();
        if self.next >= total {
            return Err("committed document capture finished".into());
        }
        let first = self.next == 0;
        let piece = match &self.base {
            Some(_) if first => CHANGES_FIRST_BYTES,
            _ => CHUNK_BYTES,
        };
        let end = total.min(self.next.saturating_add(piece));
        let bytes = self
            .body
            .bytes()
            .get(self.next..end)
            .ok_or("committed document cursor out of range")?
            .to_vec();
        let total =
            u32::try_from(total).map_err(|_| "committed document exceeds the wire limit")?;
        self.next = end;
        self.token = uuid::Uuid::new_v4().into_bytes();
        if let Some(base) = self.base.as_ref().filter(|_| first) {
            return Ok(runtime_collab::committed_document::Reply::Changes {
                capture: self.id,
                revision: self.revision,
                base: base.clone(),
                total,
                bytes,
                token: self.token,
            });
        }
        Ok(runtime_collab::committed_document::Reply::Chunk {
            capture: self.id,
            revision: self.revision,
            total,
            offset,
            bytes,
            token: self.token,
        })
    }
    pub(super) fn check_deadline(&mut self, now: std::time::Instant) -> Result<(), String> {
        self.expired |= now >= self.deadline;
        if self.expired {
            Err("committed document capture expired".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn acknowledges(&self, token: [u8; 16]) -> bool {
        self.token == token
    }
    pub(super) fn finished(&self) -> bool {
        self.next == self.body.bytes().len()
    }
}
#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        reason = "fixture offsets are a few chunks, far below u32::MAX"
    )]
    use super::*;
    struct Bytes(Vec<u8>);
    impl DocumentBytes for Bytes {
        fn bytes(&self) -> &[u8] {
            &self.0
        }
    }

    #[test]
    fn actual_empty_record_passes_the_broker_capture_and_creation_wire_codec() {
        use automerge::ReadDoc as _;
        use runtime_collab::committed_document::{
            CreationChunk, CreationReceipt, CreationWorkspace, Reply,
        };
        use tilo_peer_state::{BoundedStatePeer, PeerLimits, SeedDocument, SeedState, StatePeer};
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
        let person = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[22; 32]);
        let mut certificate = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Empty record fixture".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &person).unwrap();
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let roster = vec![certificate.clone()];
        let now = std::time::SystemTime::now();
        let mut source = StatePeer::new(
            room,
            certificate.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        source
            .install_seed(
                SeedState {
                    room_changes: tilo_peer_state::seed_room(room, &certificate, &roster).unwrap(),
                    documents: vec![SeedDocument {
                        app_id: "test.app".into(),
                        record_id: String::new(),
                        kind: "test.record".into(),
                        bytes: automerge::AutoCommit::new().save(),
                    }],
                },
                &device,
                now,
            )
            .unwrap();
        let bounded = BoundedStatePeer::restore(
            &source.durable().unwrap(),
            room,
            &certificate,
            &roster,
            &RevocationList::new(),
            now,
            &PeerLimits::default(),
        )
        .unwrap();
        let record = bounded
            .read()
            .capture_document(
                certificate.device_public_key,
                ("test.app", ""),
                now,
                64 * 1024,
                &mut |_| Err("empty plaintext record has no epoch opener".into()),
            )
            .unwrap();
        let reservation = Arc::new(Budget::default()).try_reserve().unwrap();
        let mut stream = Capture::new(
            Box::new(Bytes(record.into_bytes())),
            reservation,
            1,
            [3; 32],
        )
        .unwrap();
        let reply = Reply::Created {
            receipt: CreationReceipt {
                token: [7; 16],
                workspace: CreationWorkspace {
                    id: room.into(),
                    name: "Empty fixture room".into(),
                },
            },
            chunk: CreationChunk::from_reply(stream.chunk(0, [0; 16]).unwrap()).unwrap(),
        };
        assert_eq!(Reply::decode(&reply.encode().unwrap()).unwrap(), reply);
        let Reply::Created { chunk, .. } = reply else {
            panic!("actual creation chunk")
        };
        let document = automerge::AutoCommit::load(&chunk.bytes).unwrap();
        assert_eq!(
            document.keys(automerge::ROOT).count(),
            0,
            "capturing never seeds app content"
        );
    }
    #[test]
    fn committed_capture_reserves_before_work_and_releases_on_drop() {
        let budget = Arc::new(Budget::default());
        let first = budget.try_reserve().unwrap();
        let second = budget.try_reserve().unwrap();
        assert!(budget.try_reserve().is_none());
        drop(first);
        assert!(budget.try_reserve().is_some());
        drop(second);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn committed_capture_expiry_is_terminal_and_keeps_quota_until_owner_drops() {
        let budget = Arc::new(Budget::default());
        let second = budget.try_reserve().unwrap();
        let mut capture = Capture::new(
            Box::new(Bytes(vec![7; CHUNK_BYTES + 1])),
            budget.try_reserve().unwrap(),
            1,
            [7; 32],
        )
        .unwrap();
        let runtime_collab::committed_document::Reply::Chunk { token, .. } =
            capture.chunk(0, [0; 16]).unwrap()
        else {
            panic!("chunk")
        };
        assert!(
            !capture.acknowledges([0; 16]),
            "the first request token cannot cancel a returned reply"
        );
        assert!(
            capture.chunk(CHUNK_BYTES as u32, [0; 16]).is_err(),
            "pipelined guesses cannot consume the next body"
        );
        assert!(
            capture
                .check_deadline(std::time::Instant::now() + std::time::Duration::from_secs(6))
                .is_err()
        );
        assert!(
            capture.chunk(CHUNK_BYTES as u32, token).is_err(),
            "current token cannot revive an expired capture"
        );
        assert_eq!(
            budget.used(),
            MAX_DOCUMENT_BYTES + CHUNK_BYTES + 1,
            "expiry cannot free bytes while an unread response owner remains"
        );
        drop(capture);
        assert_eq!(budget.used(), MAX_DOCUMENT_BYTES);
        drop(second);
    }
    #[test]
    fn a_capture_waiting_to_be_read_holds_its_body_not_the_largest_body() {
        let budget = Arc::new(Budget::default());
        // Every app of a session starting at once: each read's capture waits
        // for its reader, and none keeps the next from being captured.
        let waiting: Vec<Capture> = (1..=16)
            .map(|id| {
                Capture::new(
                    Box::new(Bytes(vec![1; 4096])),
                    budget
                        .try_reserve()
                        .expect("room for another small capture"),
                    id,
                    [1; 32],
                )
                .unwrap()
            })
            .collect();
        assert_eq!(budget.used(), 16 * 4096);
        drop(waiting);
        assert_eq!(budget.used(), 0);
        // What is held is still bounded: bodies as large as a capture may be
        // fill the budget.
        let large: Vec<Capture> = (1..=2)
            .map(|id| {
                Capture::new(
                    Box::new(Bytes(vec![1; MAX_DOCUMENT_BYTES])),
                    budget.try_reserve().unwrap(),
                    id,
                    [1; 32],
                )
                .unwrap()
            })
            .collect();
        assert!(budget.try_reserve().is_none());
        drop(large);
        assert_eq!(budget.used(), 0);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reads_beyond_the_budget_wait_and_all_complete_in_order() {
        let budget = Arc::new(Budget::default());
        let held = [budget.try_reserve().unwrap(), budget.try_reserve().unwrap()];
        let served = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut waiting = Vec::new();
        for read in 0..12 {
            let turn = budget.wait();
            let served = Arc::clone(&served);
            let budget = Arc::clone(&budget);
            waiting.push(tokio::spawn(async move {
                let reservation = turn.await.unwrap();
                served.lock().unwrap().push(read);
                // The capture holds a small body for a moment, then is read.
                let capture = Capture::new(
                    Box::new(Bytes(vec![1; 4096])),
                    reservation,
                    read + 1,
                    [1; 32],
                )
                .unwrap();
                tokio::task::yield_now().await;
                drop(capture);
                drop(budget);
            }));
            // Polled once, so it joins the queue before the next read.
            tokio::task::yield_now().await;
        }
        assert!(served.lock().unwrap().is_empty(), "the budget is held");
        assert!(
            budget.try_reserve().is_none(),
            "a later read does not overtake the waiting ones"
        );
        drop(held);
        for read in waiting {
            read.await.unwrap();
        }
        assert_eq!(*served.lock().unwrap(), (0..12).collect::<Vec<u64>>());
        assert_eq!(budget.used(), 0);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn a_capture_larger_than_the_whole_budget_is_served_when_it_is_alone() {
        let budget = Arc::new(Budget::with_total(1024));
        let other = budget.try_reserve().unwrap();
        let large = budget.wait();
        let large = tokio::spawn(async move {
            let capture = Capture::new(
                Box::new(Bytes(vec![2; 4096])),
                large.await.unwrap(),
                1,
                [2; 32],
            )
            .unwrap();
            capture.body.bytes().len()
        });
        tokio::task::yield_now().await;
        assert!(
            !large.is_finished(),
            "it waits while another read holds bytes"
        );
        drop(other);
        assert_eq!(large.await.unwrap(), 4096);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn changes_start_with_a_bounded_changes_piece_and_continue_in_chunks() {
        use runtime_collab::committed_document::Reply;
        let budget = Arc::new(Budget::default());
        let original = (0..CHUNK_BYTES + 99)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        let mut capture = Capture::new(
            Box::new(Bytes(original.clone())),
            budget.try_reserve().unwrap(),
            3,
            [6; 32],
        )
        .unwrap()
        .naming(vec![[1; 32]])
        .changes_since(vec![[2; 32]])
        .unwrap();
        assert!(capture.delivers().is_none(), "nothing is handed out unread");
        let Reply::Changes {
            capture: id,
            base,
            bytes,
            token,
            total,
            ..
        } = capture.chunk(0, [0; 16]).unwrap()
        else {
            panic!("the first piece of changes names its base");
        };
        assert_eq!(
            (id, base, total as usize),
            (3, vec![[2; 32]], original.len())
        );
        assert_eq!(bytes.len(), CHANGES_FIRST_BYTES);
        let mut output = bytes;
        let mut continuation = token;
        while output.len() < original.len() {
            let Reply::Chunk { bytes, token, .. } =
                capture.chunk(output.len() as u32, continuation).unwrap()
            else {
                panic!("changes continue in chunks");
            };
            output.extend(bytes);
            continuation = token;
        }
        assert_eq!(output, original);
        let held = capture
            .delivers()
            .expect("an entirely read capture hands out its revision");
        assert_eq!((held.revision, held.heads), ([6; 32], vec![[1; 32]]));
        assert!(
            Capture::new(
                Box::new(Bytes(vec![1])),
                budget.try_reserve().unwrap(),
                4,
                [6; 32]
            )
            .unwrap()
            .changes_since(Vec::new())
            .is_err()
        );
    }

    #[test]
    fn committed_capture_is_ordered_bounded_and_keeps_exact_bytes() {
        let budget = Arc::new(Budget::default());
        let original = vec![5; CHUNK_BYTES + 17];
        let mut capture = Capture::new(
            Box::new(Bytes(original.clone())),
            budget.try_reserve().unwrap(),
            9,
            [4; 32],
        )
        .unwrap();
        assert!(capture.chunk(1, [0; 16]).is_err());
        assert!(
            capture.chunk(0, [9; 16]).is_err(),
            "a guessed continuation must not consume a chunk"
        );
        let mut output = Vec::new();
        let mut continuation = [0; 16];
        for offset in [0, CHUNK_BYTES as u32] {
            let runtime_collab::committed_document::Reply::Chunk { bytes, token, .. } =
                capture.chunk(offset, continuation).unwrap()
            else {
                panic!("chunk")
            };
            assert!(bytes.len() <= CHUNK_BYTES);
            output.extend(bytes);
            continuation = token;
        }
        assert_eq!(output, original);
        assert!(capture.finished());
        assert!(capture.chunk(original.len() as u32, continuation).is_err());
        drop(capture);
        assert_eq!(budget.used(), 0);
    }
}
