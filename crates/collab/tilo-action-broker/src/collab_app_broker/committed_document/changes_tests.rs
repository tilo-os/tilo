//! An `OpenChanges` is the Open it stands for, except that a document
//! changed since a revision this channel handed out may be answered with the
//! changes alone; every check and every acknowledgement stays the Open's.
use super::{
    Budget, HeldRevision, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
    Reading, Reply, Slot, open_read,
};
use runtime_collab::committed_document::{CHANGES_FIRST_BYTES, CHUNK_BYTES};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

const ROOM: &str = "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af";

/// An owner whose document is at one revision, named by one head.
struct Service {
    at: Mutex<([u8; 32], [u8; 32])>,
    readable: AtomicBool,
    pending: AtomicBool,
    /// Whether it answers changes at all; an owner that does not keeps the
    /// trait's default and always reads whole.
    changes: bool,
    /// A base other than the held heads, as a wrong owner would answer.
    wrong_base: AtomicBool,
    bytes: usize,
    whole: AtomicUsize,
    since: AtomicUsize,
}
impl Service {
    fn new(changes: bool, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            at: Mutex::new(([1; 32], [11; 32])),
            readable: AtomicBool::new(true),
            pending: AtomicBool::new(false),
            changes,
            wrong_base: AtomicBool::new(false),
            bytes,
            whole: AtomicUsize::new(0),
            since: AtomicUsize::new(0),
        })
    }
    fn edit(&self, revision: u8) {
        *self.at.lock().unwrap() = ([revision; 32], [revision.wrapping_add(10); 32]);
    }
    fn at(&self) -> ([u8; 32], [u8; 32]) {
        *self.at.lock().unwrap()
    }
}
struct Bytes(Vec<u8>);
impl super::DocumentBytes for Bytes {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}
impl NativeDocumentReads for Service {
    fn requires_room(&self, _: &str) -> bool {
        true
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        if self.readable.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("the reader may not read that document".into())
        }
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known: Option<[u8; 32]>,
        _: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.authorize(context)?;
        self.whole.fetch_add(1, Ordering::AcqRel);
        if self.pending.load(Ordering::Acquire) {
            return Ok(NativeDocumentRead::Pending);
        }
        let (revision, tip) = self.at();
        if Some(revision) == known {
            return Ok(NativeDocumentRead::Unchanged(revision));
        }
        Ok(NativeDocumentRead::Captured {
            revision,
            heads: vec![tip],
            body: Box::new(Bytes(vec![1; self.bytes])),
        })
    }
    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        if !self.changes {
            return self.capture(context, Some(held.revision), maximum);
        }
        self.authorize(context)?;
        self.since.fetch_add(1, Ordering::AcqRel);
        if self.pending.load(Ordering::Acquire) {
            return Ok(NativeDocumentRead::Pending);
        }
        let (revision, tip) = self.at();
        if revision == held.revision {
            return Ok(NativeDocumentRead::Unchanged(revision));
        }
        let base = if self.wrong_base.load(Ordering::Acquire) {
            vec![[99; 32]]
        } else {
            held.heads.clone()
        };
        Ok(NativeDocumentRead::Changes {
            revision,
            heads: vec![tip],
            base,
            body: Box::new(Bytes(vec![2; self.bytes])),
        })
    }
}

fn context(custody: &Arc<AtomicBool>) -> NativeDocumentReadContext {
    let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Reader".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let custody = Arc::clone(custody);
    NativeDocumentReadContext {
        route: crate::collab_broker::CrdtRoute::record("so.tilo.notes", ROOM, ""),
        certificate,
        record_kind: None,
        creation: None,
        revalidate: Arc::new(move || {
            if custody.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("committed read launcher custody ended".into())
            }
        }),
    }
}

/// One admitted channel: its slot, custody, budget and capture ids.
struct Channel {
    slot: Slot,
    custody: Arc<AtomicBool>,
    service: Arc<Service>,
    budget: Arc<Budget>,
    ids: AtomicU64,
}
impl Channel {
    fn new(service: &Arc<Service>) -> Self {
        Self::sharing(service, &Arc::new(Budget::default()))
    }
    /// A channel whose captures come out of `budget`, shared with others.
    fn sharing(service: &Arc<Service>, budget: &Arc<Budget>) -> Self {
        Self {
            slot: Slot::default(),
            custody: Arc::new(AtomicBool::new(true)),
            service: Arc::clone(service),
            budget: Arc::clone(budget),
            ids: AtomicU64::new(1),
        }
    }
    fn open(&mut self, known: Option<[u8; 32]>) -> Result<Reply, String> {
        self.request(known, None)
    }
    /// What the dispatcher does for `OpenChanges { known }`.
    fn open_changes(&mut self, known: [u8; 32]) -> Result<Reply, String> {
        let held = self.slot.held(known);
        self.request(Some(known), held.as_ref())
    }
    fn request(
        &mut self,
        known: Option<[u8; 32]>,
        held: Option<&HeldRevision>,
    ) -> Result<Reply, String> {
        let reading = Reading {
            context: context(&self.custody),
            service: self.service.clone(),
            budget: &self.budget,
            ids: &self.ids,
            prepared: None,
        };
        open_read(&mut self.slot, reading, known, held).map(|(reply, _)| reply)
    }
    /// Read every remaining piece after `first`, and the last token.
    fn read_rest(&mut self, first: &Reply) -> Result<(u64, Vec<u8>, [u8; 16]), String> {
        let (capture, total, mut bytes, mut token) = match first {
            Reply::Chunk {
                capture,
                total,
                bytes,
                token,
                ..
            }
            | Reply::Changes {
                capture,
                total,
                bytes,
                token,
                ..
            } => (*capture, *total as usize, bytes.clone(), *token),
            other => return Err(format!("no capture: {other:?}")),
        };
        while bytes.len() < total {
            let Reply::Chunk {
                bytes: piece,
                token: next,
                ..
            } = self
                .slot
                .next(capture, u32::try_from(bytes.len()).unwrap(), token)?
            else {
                return Err("a capture continues in chunks".into());
            };
            bytes.extend(piece);
            token = next;
        }
        Ok((capture, bytes, token))
    }
    /// Read a capture to its end and acknowledge it.
    fn take(&mut self, first: &Reply) -> Vec<u8> {
        let (capture, bytes, token) = self.read_rest(first).unwrap();
        assert_eq!(self.slot.acknowledge(capture, token), Ok(Reply::Cancelled));
        bytes
    }
}

#[test]
fn changes_start_only_from_a_revision_this_channel_handed_out_entirely() {
    let service = Service::new(true, 3);
    let mut channel = Channel::new(&service);
    // Before anything was handed out, OpenChanges is the Open it stands for.
    let first = channel.open_changes([1; 32]).unwrap();
    assert_eq!(first, Reply::Unchanged { revision: [1; 32] });
    let first = channel.open(None).unwrap();
    assert!(matches!(first, Reply::Chunk { revision, .. } if revision == [1; 32]));
    let (capture, _, token) = channel.read_rest(&first).unwrap();
    service.edit(2);
    assert!(
        channel.open_changes([1; 32]).is_err(),
        "an unacknowledged capture still blocks the next read"
    );
    assert_eq!(
        channel.slot.acknowledge(capture, token),
        Ok(Reply::Cancelled)
    );
    let changes = channel.open_changes([1; 32]).unwrap();
    let Reply::Changes { revision, base, .. } = &changes else {
        panic!("a changed document is answered with the changes since: {changes:?}");
    };
    assert_eq!((*revision, base.clone()), ([2; 32], vec![[11; 32]]));
    assert_eq!(channel.take(&changes), vec![2; 3]);
    // The changes became what the channel handed out.
    assert_eq!(
        channel.open_changes([2; 32]).unwrap(),
        Reply::Unchanged { revision: [2; 32] }
    );
    service.edit(3);
    let changes = channel.open_changes([2; 32]).unwrap();
    assert!(matches!(&changes, Reply::Changes { base, .. } if *base == vec![[12; 32]]));
    channel.take(&changes);
    // A revision this channel never handed out is read whole.
    service.edit(4);
    let whole = channel.open_changes([2; 32]).unwrap();
    assert!(matches!(whole, Reply::Chunk { revision, .. } if revision == [4; 32]));
    assert_eq!(service.since.load(Ordering::Acquire), 3);
}

#[test]
fn a_capture_abandoned_before_its_end_hands_out_nothing_to_start_from() {
    let service = Service::new(true, CHUNK_BYTES + 1);
    let mut channel = Channel::new(&service);
    let Reply::Chunk { capture, token, .. } = channel.open(None).unwrap() else {
        panic!("a whole first read");
    };
    // The reader gives up after the first piece (a deadline, a refusal).
    assert_eq!(
        channel.slot.acknowledge(capture, token),
        Ok(Reply::Cancelled)
    );
    service.edit(2);
    let whole = channel.open_changes([1; 32]).unwrap();
    assert!(
        matches!(whole, Reply::Chunk { revision, .. } if revision == [2; 32]),
        "a revision never entirely handed out is not a base"
    );
    assert_eq!(service.since.load(Ordering::Acquire), 0);
}

#[test]
fn every_piece_of_changes_is_authorized_and_a_revocation_while_editing_fails_closed() {
    let service = Service::new(true, CHANGES_FIRST_BYTES + 10);
    let mut channel = Channel::new(&service);
    let first = channel.open(None).unwrap();
    channel.take(&first);
    // A collaborator keeps editing; each edit is read as changes.
    for revision in 2..5 {
        service.edit(revision);
        let changes = channel.open_changes([revision - 1; 32]).unwrap();
        assert!(matches!(changes, Reply::Changes { .. }));
        channel.take(&changes);
    }
    service.edit(5);
    let changes = channel.open_changes([4; 32]).unwrap();
    let Reply::Changes {
        capture,
        bytes,
        token,
        ..
    } = &changes
    else {
        panic!("changes in more than one piece");
    };
    assert_eq!(bytes.len(), CHANGES_FIRST_BYTES);
    service.readable.store(false, Ordering::Release);
    assert!(
        channel
            .slot
            .next(*capture, u32::try_from(bytes.len()).unwrap(), *token)
            .is_err(),
        "the next piece re-checks the reader's permission"
    );
    // The reader cannot acknowledge an unread capture into a base, and its
    // next read is refused like an Open.
    assert_eq!(
        channel.slot.acknowledge(*capture, *token),
        Ok(Reply::Cancelled)
    );
    service.edit(6);
    assert!(channel.open_changes([4; 32]).is_err());
    assert!(channel.open(Some([4; 32])).is_err());
    // Ended custody refuses as well, even where the owner would answer.
    service.readable.store(true, Ordering::Release);
    channel.custody.store(false, Ordering::Release);
    assert!(channel.open_changes([4; 32]).is_err());
}

#[test]
fn a_pending_checkpoint_is_unavailable_never_changes() {
    let service = Service::new(true, 3);
    let mut channel = Channel::new(&service);
    let first = channel.open(None).unwrap();
    channel.take(&first);
    service.pending.store(true, Ordering::Release);
    assert_eq!(channel.open_changes([1; 32]).unwrap(), Reply::Unavailable);
}

#[test]
fn changes_from_another_base_are_refused() {
    let service = Service::new(true, 3);
    let mut channel = Channel::new(&service);
    let first = channel.open(None).unwrap();
    channel.take(&first);
    service.edit(2);
    service.wrong_base.store(true, Ordering::Release);
    assert!(
        channel.open_changes([1; 32]).is_err(),
        "changes that do not start from the held heads never reach the reader"
    );
    assert!(channel.slot.active.is_none());
}

#[test]
fn an_owner_that_reads_only_whole_documents_keeps_answering_whole() {
    let service = Service::new(false, 3);
    let mut channel = Channel::new(&service);
    let first = channel.open(None).unwrap();
    channel.take(&first);
    service.edit(2);
    let whole = channel.open_changes([1; 32]).unwrap();
    assert!(matches!(whole, Reply::Chunk { revision, .. } if revision == [2; 32]));
    assert_eq!(service.whole.load(Ordering::Acquire), 2);
}

/// Run `test` on a local set, as the broker runs its channels.
fn on_the_broker(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(test));
}

/// Let the queue's tasks take their places and hand out what was released.
async fn settle() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn reads_beyond_the_budget_wait_their_turn_and_all_complete_in_order() {
    use runtime_collab::committed_document::MAX_DOCUMENT_BYTES;
    on_the_broker(async {
        // Room for one capture at a time: every other read must wait.
        let budget = Arc::new(Budget::with_total(MAX_DOCUMENT_BYTES));
        let service = Service::new(false, 3);
        let mut channels: Vec<Channel> = (0..6)
            .map(|_| Channel::sharing(&service, &budget))
            .collect();
        let mut firsts = Vec::new();
        for channel in &mut channels {
            firsts.push(channel.open(None).unwrap());
            settle().await;
        }
        assert!(matches!(firsts[0], Reply::Chunk { .. }));
        assert!(
            firsts[1..].iter().all(|reply| *reply == Reply::Busy),
            "the rest wait, refused by nothing: {firsts:?}"
        );
        let mut first = firsts.remove(0);
        for turn in 0..channels.len() {
            assert_eq!(channels[turn].take(&first), vec![1; 3]);
            settle().await;
            for later in channels.iter_mut().skip(turn + 2) {
                assert_eq!(
                    later.open(None).unwrap(),
                    Reply::Busy,
                    "a later read does not overtake an earlier one"
                );
            }
            if let Some(next) = channels.get_mut(turn + 1) {
                first = next.open(None).unwrap();
                assert!(
                    matches!(first, Reply::Chunk { .. }),
                    "read {} is served once read {turn} is done: {first:?}",
                    turn + 1
                );
            }
        }
        drop(channels);
        settle().await;
        assert_eq!(budget.used(), 0);
    });
}

#[test]
fn a_read_larger_than_the_whole_budget_completes() {
    on_the_broker(async {
        let budget = Arc::new(Budget::with_total(1024));
        let service = Service::new(false, 4096);
        let mut large = Channel::sharing(&service, &budget);
        let mut other = Channel::sharing(&service, &budget);
        let first = large.open(None).unwrap();
        assert_eq!(large.take(&first), vec![1; 4096]);
        // While it is held, others wait; once it is read, they are served.
        let first = large.open(None).unwrap();
        assert_eq!(other.open(None).unwrap(), Reply::Busy);
        large.take(&first);
        settle().await;
        let first = other.open(None).unwrap();
        assert_eq!(other.take(&first), vec![1; 4096]);
    });
}

#[test]
fn a_refusal_of_authority_stays_final_whatever_the_budget_holds() {
    use runtime_collab::committed_document::MAX_DOCUMENT_BYTES;
    on_the_broker(async {
        let budget = Arc::new(Budget::with_total(MAX_DOCUMENT_BYTES));
        let service = Service::new(false, 3);
        let mut holder = Channel::sharing(&service, &budget);
        let held = holder.open(None).unwrap();
        // A reader whose custody ended is refused, not queued.
        let mut revoked = Channel::sharing(&service, &budget);
        revoked.custody.store(false, Ordering::Release);
        assert!(revoked.open(None).is_err());
        // A reader queued behind the budget whose owner then refuses it is
        // refused once its turn comes.
        let mut queued = Channel::sharing(&service, &budget);
        assert_eq!(queued.open(None).unwrap(), Reply::Busy);
        service.readable.store(false, Ordering::Release);
        holder.take(&held);
        settle().await;
        assert!(queued.open(None).is_err());
        // The refused read released its turn.
        service.readable.store(true, Ordering::Release);
        let first = holder.open(None).unwrap();
        assert!(matches!(first, Reply::Chunk { .. }));
    });
}

#[test]
fn a_queued_read_refused_or_left_gives_up_its_place() {
    use runtime_collab::committed_document::MAX_DOCUMENT_BYTES;
    on_the_broker(async {
        let budget = Arc::new(Budget::with_total(MAX_DOCUMENT_BYTES));
        let service = Service::new(false, 3);
        let mut holder = Channel::sharing(&service, &budget);
        let held = holder.open(None).unwrap();
        let mut revoked = Channel::sharing(&service, &budget);
        let mut leaving = Channel::sharing(&service, &budget);
        let mut last = Channel::sharing(&service, &budget);
        for channel in [&mut revoked, &mut leaving, &mut last] {
            assert_eq!(channel.open(None).unwrap(), Reply::Busy);
            settle().await;
        }
        // The first in line loses custody while it waits; the second stops
        // reading on its channel.
        revoked.custody.store(false, Ordering::Release);
        leaving.slot.leave();
        holder.take(&held);
        settle().await;
        // The refusal is final, and the room granted to it goes on.
        assert!(revoked.open(None).is_err());
        settle().await;
        let first = last.open(None).unwrap();
        assert!(
            matches!(first, Reply::Chunk { .. }),
            "the read behind a refused and a departed one is served: {first:?}"
        );
        assert_eq!(last.take(&first), vec![1; 3]);
        drop((holder, revoked, leaving, last));
        settle().await;
        assert_eq!(budget.used(), 0);
    });
}
