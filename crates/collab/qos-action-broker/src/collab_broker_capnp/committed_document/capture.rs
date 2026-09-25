//! Byte custody for immutable native reads. Reservations precede capture work.
use runtime_collab::committed_document::{CHUNK_BYTES, MAX_DOCUMENT_BYTES};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// The actual bounded native save remains owned by its provider.
pub trait DocumentBytes: Send + Sync {
    fn bytes(&self) -> &[u8];
}
#[derive(Default)]
pub(crate) struct Budget(AtomicUsize);
pub(super) struct Reservation {
    budget: Arc<Budget>,
}
impl Budget {
    pub(super) fn reserve(self: &Arc<Self>) -> Result<Reservation, String> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(MAX_DOCUMENT_BYTES)
                    .filter(|next| *next <= TOTAL_BYTES)
            })
            .map_err(|_| "committed document capture capacity exhausted".to_string())?;
        Ok(Reservation {
            budget: self.clone(),
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget
            .0
            .fetch_sub(MAX_DOCUMENT_BYTES, Ordering::AcqRel);
    }
}
pub(super) struct Capture {
    body: Box<dyn DocumentBytes>,
    // Drop body (and its native charges) before releasing this quota.
    _reservation: Reservation,
    pub(super) id: u64,
    pub(super) revision: [u8; 32],
    next: usize,
    token: [u8; 16],
    deadline: std::time::Instant,
    expired: bool,
}
impl Capture {
    pub(super) fn new(
        body: Box<dyn DocumentBytes>,
        reservation: Reservation,
        id: u64,
        revision: [u8; 32],
    ) -> Result<Self, String> {
        let len = body.bytes().len();
        if len == 0 || len > MAX_DOCUMENT_BYTES || id == 0 {
            return Err("invalid native document capture".into());
        }
        let now = std::time::Instant::now();
        Ok(Self {
            body,
            _reservation: reservation,
            id,
            revision,
            next: 0,
            token: [0; 16],
            deadline: now
                .checked_add(std::time::Duration::from_secs(5))
                .unwrap_or(now),
            expired: false,
        })
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
        let end = total.min(self.next.saturating_add(CHUNK_BYTES));
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
    #[cfg(test)]
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
    fn committed_capture_reserves_before_work_and_releases_on_drop() {
        let budget = Arc::new(Budget::default());
        let first = budget.reserve().unwrap();
        let second = budget.reserve().unwrap();
        assert!(budget.reserve().is_err());
        drop(first);
        assert!(budget.reserve().is_ok());
        drop(second);
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
    }
    #[test]
    fn committed_capture_expiry_is_terminal_and_keeps_quota_until_owner_drops() {
        let budget = Arc::new(Budget::default());
        let second = budget.reserve().unwrap();
        let mut capture = Capture::new(
            Box::new(Bytes(vec![7; CHUNK_BYTES + 1])),
            budget.reserve().unwrap(),
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
        assert!(
            budget.reserve().is_err(),
            "expiry cannot free bytes while an unread response owner remains"
        );
        drop(capture);
        assert!(budget.reserve().is_ok());
        drop(second);
    }
    #[test]
    fn committed_capture_is_ordered_bounded_and_keeps_exact_bytes() {
        let budget = Arc::new(Budget::default());
        let original = vec![5; CHUNK_BYTES + 17];
        let mut capture = Capture::new(
            Box::new(Bytes(original.clone())),
            budget.reserve().unwrap(),
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
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
    }
}
