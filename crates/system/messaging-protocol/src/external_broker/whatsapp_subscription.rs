//! Bounded, account-owned SDK recipient receipt subscription.
use super::{Arc, ExternalBroker, SendError};
use std::sync::{
    Weak,
    atomic::{AtomicBool, Ordering},
};
use whatsapp_rust::{
    Client,
    async_channel::{Receiver, Sender},
    wacore::types::events::{Event, EventHandler, EventInterest, EventKind, Subscription},
};

const CAPACITY: usize = 64;
struct Handler {
    sender: Sender<Arc<Event>>,
    lost: Arc<AtomicBool>,
}
impl EventHandler for Handler {
    fn interest(&self) -> EventInterest {
        EventInterest::of(&[EventKind::Receipt])
    }
    fn handle_event(&self, event: Arc<Event>) {
        let Event::Receipt(receipt) = &*event else {
            return;
        };
        // Bound metadata before retaining the SDK event. No message bodies or
        // credentials enter this queue. Unsupported evidence is never promoted.
        let bounded = !receipt.message_ids.is_empty()
            && receipt.message_ids.len() <= CAPACITY
            && receipt.message_ids.iter().all(|id| crate::identifier(id))
            && receipt.source.chat.to_string().len() <= 256
            && receipt.source.sender.to_string().len() <= 256
            && receipt.r#type.as_wire_str().len() <= 64
            && [
                &receipt.source.sender_alt,
                &receipt.source.recipient_alt,
                &receipt.source.broadcast_list_owner,
                &receipt.source.recipient,
            ]
            .into_iter()
            .flatten()
            .all(|jid| jid.to_string().len() <= 256);
        if !bounded || self.sender.try_send(event).is_err() {
            // Never silently discard a receipt and advertise a healthy stream.
            // The supervisor must replace this subscription after reconciliation.
            self.lost.store(true, Ordering::Release);
            self.sender.close();
        }
    }
}

/// Retain on the account supervisor and drain with `next`. Drop unregisters the
/// SDK handler. Overflow is terminal and observable; it creates no delivered
/// state, reconnect, send or replay. This is not a durable provider event queue.
pub struct WhatsappReceiptSubscription {
    broker: Weak<ExternalBroker>,
    source: Arc<Client>,
    receiver: Receiver<Arc<Event>>,
    handler: Arc<Handler>,
    failed: bool,
    _subscription: Subscription,
}
impl WhatsappReceiptSubscription {
    /// Register only on the precise SDK owned by this `WhatsApp` broker, before
    /// connecting it. Holding a client is not credential or Contacts authority.
    /// # Errors
    /// Refuses foreign clients and brokers for another service.
    pub async fn attach(
        broker: &Arc<ExternalBroker>,
        source: Arc<Client>,
    ) -> Result<Self, SendError> {
        if broker.owner.service != crate::Service::Whatsapp {
            return Err(SendError::Revoked);
        }
        let worker = broker.worker.lock().await;
        if !worker.adapter.whatsapp_source(&source) {
            return Err(SendError::Revoked);
        }
        drop(worker);
        let (sender, receiver) = whatsapp_rust::async_channel::bounded(CAPACITY);
        let lost = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(Handler { sender, lost });
        let subscription = source.subscribe_handler(handler.clone());
        Ok(Self {
            broker: Arc::downgrade(broker),
            source,
            receiver,
            handler,
            failed: false,
            _subscription: subscription,
        })
    }
    /// Process one event with the existing exact operation/route/current grant
    /// checks. Unknown operations do not create history. A batched receipt may
    /// persist known operations individually, never invent evidence for others.
    /// # Errors
    /// Refuses failed streams, supervisor loss, revoked identities and failed
    /// persistence. The supervisor owns retry/reconciliation policy.
    pub async fn next(&mut self) -> Result<usize, SendError> {
        let result = self.next_event().await;
        if result.is_err() {
            self.failed = true;
            self.handler.sender.close();
        }
        result
    }
    async fn next_event(&self) -> Result<usize, SendError> {
        if self.failed || self.handler.lost.load(Ordering::Acquire) {
            return Err(SendError::Unavailable);
        }
        let event = self
            .receiver
            .recv()
            .await
            .map_err(|_| SendError::Unavailable)?;
        if self.failed || self.handler.lost.load(Ordering::Acquire) {
            return Err(SendError::Unavailable);
        }
        let broker = self.broker.upgrade().ok_or(SendError::Unavailable)?;
        let Event::Receipt(receipt) = &*event else {
            return Err(SendError::InvalidRequest);
        };
        let mut changed = 0usize;
        for operation in &receipt.message_ids {
            match broker
                .observe_whatsapp_receipt(operation.clone(), self.source.clone(), receipt.clone())
                .await
            {
                Ok(true) => changed = changed.saturating_add(1),
                Ok(false) | Err(SendError::InvalidRequest) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(changed)
    }
}

impl Drop for WhatsappReceiptSubscription {
    fn drop(&mut self) {
        self.handler.sender.close();
    }
}

#[cfg(test)]
mod tests;
