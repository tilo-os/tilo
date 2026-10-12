//! A held `Await` makes exactly the Open's checks, and makes them again only
//! when told something changed (or, where nothing can tell it, on the clock
//! readers used to poll at).
use super::{
    Budget, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads, Reply,
    UNANNOUNCED_RECHECK, hold_until_stale, next_unannounced_recheck, read_unchanged,
};
use std::cell::Cell;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

const ROOM: &str = "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af";

/// What the owner would answer an Open now.
#[derive(Clone, Copy)]
enum Owner {
    At([u8; 32]),
    Pending,
}

struct Service {
    owner: Mutex<Owner>,
    readable: AtomicBool,
    /// Whether `unchanged` can answer from heads; otherwise the broker
    /// captures.
    cheap: bool,
    captures: AtomicUsize,
}
impl Service {
    fn new(owner: Owner, cheap: bool) -> Arc<Self> {
        Arc::new(Self {
            owner: Mutex::new(owner),
            readable: AtomicBool::new(true),
            cheap,
            captures: AtomicUsize::new(0),
        })
    }
    fn owner(&self) -> Owner {
        *self.owner.lock().unwrap()
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
        self.captures.fetch_add(1, Ordering::AcqRel);
        Ok(match self.owner() {
            Owner::At(revision) if Some(revision) == known => {
                NativeDocumentRead::Unchanged(revision)
            }
            Owner::At(revision) => NativeDocumentRead::Captured {
                revision,
                heads: Vec::new(),
                body: Box::new(Bytes(vec![1])),
            },
            Owner::Pending => NativeDocumentRead::Pending,
        })
    }
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        if !self.cheap {
            return Ok(None);
        }
        self.authorize(context)?;
        Ok(match (self.owner(), known) {
            (Owner::At(revision), Some(known)) => Some(revision == known),
            _ => None,
        })
    }
}

struct Bytes(Vec<u8>);
impl super::DocumentBytes for Bytes {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

fn context(custody: &Arc<AtomicBool>) -> NativeDocumentReadContext {
    let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
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

fn unchanged(service: &Service, custody: &Arc<AtomicBool>, known: Option<[u8; 32]>) -> bool {
    read_unchanged(
        service,
        || Ok(context(custody)),
        &Arc::new(Budget::default()),
        ROOM,
        known,
    )
    .is_some()
}

#[test]
fn a_wait_holds_exactly_while_an_open_would_answer_what_the_reader_holds() {
    for cheap in [true, false] {
        let custody = Arc::new(AtomicBool::new(true));
        let service = Service::new(Owner::At([1; 32]), cheap);
        assert!(unchanged(&service, &custody, Some([1; 32])));
        assert!(
            !unchanged(&service, &custody, Some([2; 32])),
            "a reader holding another revision is stale"
        );
        assert!(
            !unchanged(&service, &custody, None),
            "a readable record is stale to a reader that holds nothing"
        );
        *service.owner.lock().unwrap() = Owner::At([3; 32]);
        assert!(
            !unchanged(&service, &custody, Some([1; 32])),
            "a newer committed revision ends the wait"
        );
        *service.owner.lock().unwrap() = Owner::Pending;
        assert!(
            unchanged(&service, &custody, None),
            "a checkpoint still arriving holds a reader that holds nothing"
        );
    }
}

#[test]
fn a_refusal_ends_the_wait_so_the_reader_s_open_meets_it() {
    let custody = Arc::new(AtomicBool::new(true));
    let service = Service::new(Owner::At([1; 32]), true);
    service.readable.store(false, Ordering::Release);
    assert!(
        !unchanged(&service, &custody, Some([1; 32])),
        "a revoked read permission is never held"
    );
    service.readable.store(true, Ordering::Release);
    custody.store(false, Ordering::Release);
    assert!(
        !unchanged(&service, &custody, Some([1; 32])),
        "ended launcher custody is never held"
    );
    assert!(
        read_unchanged(
            service.as_ref(),
            || Err("committed read admission ended".into()),
            &Arc::new(Budget::default()),
            ROOM,
            Some([1; 32]),
        )
        .is_none(),
        "a refused admission is never held"
    );
}

#[test]
fn an_unchanged_revision_is_held_from_heads_without_a_capture() {
    let custody = Arc::new(AtomicBool::new(true));
    let service = Service::new(Owner::At([1; 32]), true);
    for _ in 0..10 {
        assert!(unchanged(&service, &custody, Some([1; 32])));
    }
    assert_eq!(service.captures.load(Ordering::Acquire), 0);
}

#[test]
fn a_capture_the_owner_cannot_avoid_respects_the_shared_byte_budget() {
    let custody = Arc::new(AtomicBool::new(true));
    let service = Service::new(Owner::At([1; 32]), false);
    let budget = Arc::new(Budget::default());
    let held: Vec<_> = std::iter::from_fn(|| budget.reserve().ok()).collect();
    assert!(!held.is_empty());
    assert!(
        read_unchanged(
            service.as_ref(),
            || Ok(context(&custody)),
            &budget,
            ROOM,
            Some([1; 32]),
        )
        .is_none(),
        "with the budget spent the wait answers Stale and the Open decides"
    );
    assert_eq!(service.captures.load(Ordering::Acquire), 0);
    drop(held);
    assert!(
        read_unchanged(
            service.as_ref(),
            || Ok(context(&custody)),
            &budget,
            ROOM,
            Some([1; 32]),
        )
        .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn an_announced_wait_checks_again_only_when_woken() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let checks = Cell::new(0_u32);
    let stale = Cell::new(false);
    let held = hold_until_stale(
        &wake,
        || {},
        || {
            checks.set(checks.get() + 1);
            Ok((!stale.get()).then_some(true))
        },
    );
    tokio::pin!(held);
    // A minute of idle time: one check, no clock.
    assert!(
        tokio::time::timeout(Duration::from_secs(60), held.as_mut())
            .await
            .is_err()
    );
    assert_eq!(checks.get(), 1, "an idle wait never re-checks on a clock");
    // Something it reads changed, but not the answer: one more check.
    wake.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(60), held.as_mut())
            .await
            .is_err()
    );
    assert_eq!(checks.get(), 2);
    // The next change ends it at once.
    stale.set(true);
    let started = tokio::time::Instant::now();
    wake.notify_one();
    assert_eq!(held.await, Ok(Reply::Stale));
    assert_eq!(checks.get(), 3);
    assert!(started.elapsed() < UNANNOUNCED_RECHECK);
}

#[tokio::test(start_paused = true)]
async fn an_unannounced_input_keeps_the_old_poll_interval() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let checks = Cell::new(0_u32);
    let started = tokio::time::Instant::now();
    let held = hold_until_stale(
        &wake,
        || {},
        || {
            checks.set(checks.get() + 1);
            // Revoked on the sixth look, which a 200 ms clock reaches at 1 s.
            Ok((checks.get() < 6).then_some(false))
        },
    );
    assert_eq!(held.await, Ok(Reply::Stale));
    assert_eq!(checks.get(), 6);
    assert!(
        started.elapsed() <= UNANNOUNCED_RECHECK * 5,
        "a revocation nothing announces is still seen within the old interval"
    );
}

#[tokio::test(start_paused = true)]
async fn a_change_while_checking_is_not_missed() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let checks = Cell::new(0_u32);
    let held = hold_until_stale(
        &wake,
        || {},
        || {
            checks.set(checks.get() + 1);
            if checks.get() == 1 {
                // An announcement lands after the watch was armed and while
                // the check runs: it must wake the next turn.
                wake.notify_one();
                return Ok(Some(true));
            }
            Ok(None)
        },
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), held).await,
        Ok(Ok(Reply::Stale))
    );
    assert_eq!(checks.get(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_replaced_wait_is_refused() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let replaced = Cell::new(false);
    let held = hold_until_stale(
        &wake,
        || {},
        || {
            if replaced.get() {
                return Err("committed read wait was replaced".into());
            }
            Ok(Some(true))
        },
    );
    tokio::pin!(held);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), held.as_mut())
            .await
            .is_err()
    );
    replaced.set(true);
    wake.notify_one();
    assert!(held.await.is_err());
}

#[tokio::test(start_paused = true)]
async fn clocked_waits_share_their_recheck_ticks() {
    let first = next_unannounced_recheck();
    let now = tokio::time::Instant::now();
    assert!(first > now && first <= now.checked_add(UNANNOUNCED_RECHECK).unwrap());
    // A wait armed later within the same period re-checks on the same tick,
    // so many readers cost the broker one wakeup per tick.
    tokio::time::advance(Duration::from_millis(50)).await;
    if tokio::time::Instant::now() < first {
        assert_eq!(next_unannounced_recheck(), first);
    }
}
