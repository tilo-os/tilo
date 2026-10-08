//! One original App generation owns each native invocation until dispatch.
use super::{Arc, AtomicBool, Instant, Ordering, committed_document};
use crate::collaboration::{DocumentOwnerRetirementReceipt, DocumentOwnerScope};
use std::sync::{Mutex, atomic::AtomicU8};
use tokio::sync::Notify;

#[derive(Debug)]
pub(super) struct Generation {
    pub(super) scope: DocumentOwnerScope,
    active: Mutex<bool>,
    changed: Notify,
}
impl Generation {
    pub(super) fn new(scope: DocumentOwnerScope) -> Arc<Self> {
        Arc::new(Self {
            scope,
            active: Mutex::new(true),
            changed: Notify::new(),
        })
    }
    pub(super) fn current(&self) -> bool {
        self.active.lock().is_ok_and(|active| *active)
    }
    pub(super) fn retire(&self) -> DocumentOwnerRetirementReceipt {
        if let Ok(mut active) = self.active.lock() {
            *active = false;
        }
        self.changed.notify_waiters();
        DocumentOwnerRetirementReceipt {
            scope: self.scope.clone(),
        }
    }
    pub(super) async fn retired(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.current() {
                return;
            }
            notified.await;
        }
    }
}
const PENDING: u8 = 0;
const CLAIMED: u8 = 1;
const RETIRED: u8 = 2;

pub(super) struct InvocationDelivery {
    owner: Arc<Generation>,
    reads: committed_document::Reads,
    key: String,
    channel: Arc<AtomicBool>,
    state: AtomicU8,
    changed: Notify,
}
impl InvocationDelivery {
    pub(super) fn new(
        owner: Arc<Generation>,
        reads: committed_document::Reads,
        key: String,
        channel: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            reads,
            key,
            channel,
            state: AtomicU8::new(PENDING),
            changed: Notify::new(),
        })
    }
    pub(super) fn claim(&self) -> bool {
        if !self
            .reads
            .document_owner_scope()
            .is_ok_and(|current| Arc::ptr_eq(&current, &self.owner))
        {
            return false;
        }
        let Ok(active) = self.owner.active.lock() else {
            return false;
        };
        if !*active || !self.reads.current(&self.key, &self.channel) {
            return false;
        }
        let claimed = self
            .state
            .compare_exchange(PENDING, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if claimed {
            self.changed.notify_waiters();
        }
        claimed
    }
    pub(super) fn retire_pending(&self) {
        if self
            .state
            .compare_exchange(PENDING, RETIRED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.changed.notify_waiters();
        }
    }
    pub(super) async fn wait_for_claim(&self, deadline: Instant) -> bool {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.state.load(Ordering::Acquire) {
                CLAIMED => return true,
                RETIRED => return false,
                _ => {}
            }
            tokio::select! {
                () = &mut notified => {},
                () = self.owner.retired() => self.retire_pending(),
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => self.retire_pending(),
            }
        }
    }
}

/// Moving this guard transfers the exact pending delivery, never its authority.
pub(super) struct DeliveryCustody(pub(super) Arc<InvocationDelivery>);
impl Drop for DeliveryCustody {
    fn drop(&mut self) {
        self.0.retire_pending();
    }
}

/// The original worker owes these observed jobs a pre-dispatch receipt.
/// This custody is local to one transport, and conveys no action authority.
#[derive(Default)]
pub(super) struct PendingReceipts {
    pub(super) count: std::sync::atomic::AtomicUsize,
    changed: Notify,
}
impl PendingReceipts {
    pub(super) fn begin(self: &Arc<Self>) -> PendingReceipt {
        self.count.fetch_add(1, Ordering::AcqRel);
        PendingReceipt(self.clone())
    }
    pub(super) async fn drained(&self, deadline: Instant) -> bool {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.count.load(Ordering::Acquire) == 0 {
                return true;
            }
            if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
                .await
                .is_err()
            {
                return false;
            }
        }
    }
}
pub(super) struct PendingReceipt(Arc<PendingReceipts>);
impl Drop for PendingReceipt {
    fn drop(&mut self) {
        self.0.count.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (committed_document::Reads, Arc<Generation>, Arc<AtomicBool>) {
        let reads = committed_document::Reads::default();
        let channel = reads.close_for_test("record");
        channel.store(true, Ordering::Release);
        reads.set_document_owner_scope(Some(DocumentOwnerScope::for_keys(vec!["record".into()])));
        let owner = reads.document_owner_scope().unwrap();
        (reads, owner, channel)
    }
    #[tokio::test]
    async fn retired_owner_is_not_replaced_by_equal_registered_routes() {
        let (reads, owner, channel) = fixture();
        let delivery = InvocationDelivery::new(
            owner.clone(),
            reads.clone(),
            "record".into(),
            channel.clone(),
        );
        let receipt = reads.set_document_owner_scope(None).unwrap();
        assert_eq!(receipt.retired_scope(), &owner.scope);
        reads.set_document_owner_scope(Some(owner.scope.clone()));
        assert!(reads.publishable("record"));
        assert!(!delivery.claim());
        assert!(
            !delivery
                .wait_for_claim(crate::time::now() + super::super::Duration::from_secs(3))
                .await
        );
        let replacement = InvocationDelivery::new(
            reads.document_owner_scope().unwrap(),
            reads,
            "record".into(),
            channel,
        );
        assert!(replacement.claim());
    }
    #[tokio::test]
    async fn dropping_original_queue_custody_retires_only_pending_delivery() {
        let (reads, owner, channel) = fixture();
        let delivery = InvocationDelivery::new(owner.clone(), reads, "record".into(), channel);
        drop(DeliveryCustody(delivery.clone()));
        assert!(owner.current());
        assert!(!delivery.claim());
        assert!(
            !delivery
                .wait_for_claim(crate::time::now() + super::super::Duration::from_secs(3))
                .await
        );
    }
    #[tokio::test]
    async fn retirement_wakes_delayed_delivery_without_owner_poll_or_timer() {
        let (reads, owner, channel) = fixture();
        let delivery = InvocationDelivery::new(owner.clone(), reads, "record".into(), channel);
        let (claimed, receipt) = tokio::join!(
            delivery.wait_for_claim(crate::time::now() + super::super::Duration::from_secs(3)),
            async { owner.retire() },
        );
        assert!(!claimed);
        assert_eq!(receipt.retired_scope(), &owner.scope);
        assert!(!delivery.claim());
    }
    #[tokio::test]
    async fn claimed_delivery_is_not_reclassified_as_never_dispatched() {
        let (reads, owner, channel) = fixture();
        let delivery = InvocationDelivery::new(owner.clone(), reads, "record".into(), channel);
        let custody = DeliveryCustody(delivery.clone());
        assert!(delivery.claim());
        owner.retire();
        drop(custody);
        assert!(delivery.wait_for_claim(crate::time::now()).await);
        assert!(
            !delivery.claim(),
            "only one dispatch owner can claim an accepted job"
        );
    }
    #[tokio::test]
    async fn overdue_delivery_cannot_be_claimed_by_later_ready_owner() {
        let (reads, owner, channel) = fixture();
        let delivery =
            InvocationDelivery::new(owner.clone(), reads.clone(), "record".into(), channel);
        assert!(!delivery.wait_for_claim(crate::time::now()).await);
        assert!(!delivery.claim());
        assert!(owner.current());
        assert!(reads.publishable("record"));
    }
}
