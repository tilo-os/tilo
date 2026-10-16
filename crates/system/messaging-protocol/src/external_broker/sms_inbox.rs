//! Owned native SMS inbox observer. No mutation, ACK, discovery or reconnect.
use super::ExternalBroker;
use crate::sms_runtime::SmsInbox;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};
use tilo_messaging::SendError;

/// Retaining the guard owns this account's independent receive worker.
/// Drop interrupts only its own task and releases its D-Bus match subscriptions.
pub struct SmsReceiveGuard {
    task: tokio::task::JoinHandle<()>,
    stopped: Arc<AtomicBool>,
}
impl SmsReceiveGuard {
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}
impl Drop for SmsReceiveGuard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}
struct Stopped {
    stopped: Arc<AtomicBool>,
    observers: Weak<crate::signal_broker::observers::Observers>,
}
impl Drop for Stopped {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(observers) = self.observers.upgrade() {
            observers.publish();
        }
    }
}
impl ExternalBroker {
    /// Subscribe before initial enumeration, durably project supported known
    /// recipients and keep observing Added and native SMS property changes.
    /// The supplied adapter owns the pinned bus/modem/SIM; there is no ambient
    /// discovery. Unknown recipients receive no fabricated association.
    /// # Errors
    /// Refuses another service/owner, stale native custody, missing worker,
    /// excessive native inbox or non-durable initial persistence.
    pub async fn start_sms_receiver(&self) -> Result<SmsReceiveGuard, SendError> {
        if self.owner.service != crate::Service::Sms {
            return Err(SendError::Unavailable);
        }
        let handle = tokio::runtime::Handle::try_current().map_err(|_| SendError::Unavailable)?;
        let source = self
            .worker
            .lock()
            .await
            .adapter
            .sms_inbox()
            .ok_or(SendError::Unavailable)?;
        if source.owner() != &self.owner {
            return Err(SendError::Revoked);
        }
        let mut signals =
            tokio::time::timeout(tilo_clock::Duration::from_secs(10), source.subscribe())
                .await
                .map_err(|_| SendError::Unavailable)?
                .map_err(|_| SendError::Unavailable)?;
        self.scan_sms_inbox(&source).await?;
        let stopped = Arc::new(AtomicBool::new(false));
        let marker = Stopped {
            stopped: stopped.clone(),
            observers: Arc::downgrade(&self.observers),
        };
        let broker = self.clone();
        let task = handle.spawn(async move {
            let _marker = marker;
            let mut refresh = tokio::time::interval(tilo_clock::Duration::from_secs(30));
            refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            refresh.tick().await;
            loop {
                let result = tokio::select! {
                    event = signals.changed() => event.map_err(|_| SendError::Unavailable),
                    _ = refresh.tick() => Ok(()),
                };
                if result.is_err() || broker.scan_sms_inbox(&source).await.is_err() {
                    break;
                }
            }
        });
        Ok(SmsReceiveGuard { task, stopped })
    }
    async fn scan_sms_inbox(&self, source: &SmsInbox) -> Result<(), SendError> {
        // Bound the complete scan as well as individual broker reads. D-Bus
        // signals are drained independently while native reads are in flight.
        tokio::time::timeout(tilo_clock::Duration::from_secs(20), async {
            let paths = source.objects().await.map_err(|_| SendError::Unavailable)?;
            for path in paths {
                let Some(recipient) = source
                    .recipient(&path)
                    .await
                    .map_err(|_| SendError::Unavailable)?
                else {
                    continue;
                };
                let auth = match self.authority.incoming(&recipient) {
                    Ok(auth) => auth,
                    Err(SendError::Revoked | SendError::Unavailable) => continue,
                    Err(error) => return Err(error),
                };
                if auth.route.recipient != recipient {
                    return Err(SendError::Revoked);
                }
                match self.observe_sms_message(auth.route.clone(), path).await {
                    Ok(_) | Err(SendError::Revoked) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| SendError::Unavailable)?
    }
}
