use super::{Arc, Authorization, ExternalBroker, SendError};
use crate::{
    signal_broker::history::{GroupIncoming, IncomingRecord},
    supervisor::AccountAuthority,
};
use whatsapp_rust::{Client, wacore::types::events::InboundMessage};
impl ExternalBroker {
    pub(crate) async fn observe_mixed(
        &self,
        source: Arc<Client>,
        events: Vec<InboundMessage>,
        groups: Vec<GroupIncoming>,
        authority: Option<Arc<dyn AccountAuthority>>,
    ) -> Result<usize, SendError> {
        let group_count = events.iter().filter(|e| e.info.source.is_group).count();
        if self.owner.service != crate::Service::Whatsapp
            || events.is_empty()
            || events.len() > 64
            || group_count != groups.len()
            || (group_count > 0 && authority.is_none())
        {
            return Err(SendError::InvalidRequest);
        }
        let broker = self.clone();
        tokio::task::spawn_blocking(move || {
            check(authority.as_deref())?;
            let mut worker = broker.worker.blocking_lock();
            if !worker.adapter.whatsapp_source(&source) {
                return Err(SendError::Revoked);
            }
            let mut projected = Vec::new();
            for event in events.iter().filter(|e| !e.info.source.is_group) {
                let auth = broker.incoming_authorization(event)?;
                let native = crate::Route::new(
                    crate::Service::Whatsapp,
                    broker.owner.account.clone(),
                    auth.route.recipient.clone(),
                )
                .map_err(|_| SendError::Revoked)?;
                let incoming = worker.adapter.whatsapp_incoming(
                    &crate::whatsapp_runtime::IncomingContext {
                        accounts: &auth.accounts,
                        binding: &auth.binding,
                        source: &source,
                        route: &native,
                    },
                    event,
                )?;
                projected.push((auth, incoming));
            }
            for (auth, _) in &mut projected {
                *auth = broker.authorized(&auth.route)?;
            }
            let direct: Vec<_> = projected
                .iter()
                .map(|(auth, incoming)| IncomingRecord {
                    route: &auth.route,
                    id: &incoming.id,
                    text: &incoming.text,
                    timestamp_ms: incoming.timestamp_ms,
                })
                .collect();
            check(authority.as_deref())?;
            let count = if groups.is_empty() {
                worker.history.incoming_batch(&direct)?
            } else {
                worker.history.incoming_mixed(&direct, &groups)?
            };
            check(authority.as_deref())?;
            drop(worker);
            if count > 0 {
                broker.observers.publish();
            }
            Ok(count)
        })
        .await
        .map_err(|_| SendError::Unavailable)?
    }
    fn incoming_authorization(&self, event: &InboundMessage) -> Result<Authorization, SendError> {
        let recipient = event.info.source.sender.to_string();
        let proposed = self.authority.incoming(&recipient)?;
        let auth = self.authorized(&proposed.route)?;
        if auth.route.recipient != recipient {
            return Err(SendError::Revoked);
        }
        Ok(auth)
    }
    pub(crate) async fn group_history(
        &self,
        group: String,
        authority: Arc<dyn AccountAuthority>,
    ) -> Result<Vec<GroupIncoming>, SendError> {
        let broker = self.clone();
        tokio::task::spawn_blocking(move || {
            authority.check()?;
            let worker = broker.worker.blocking_lock();
            let rows = worker.history.group_inbox(&group)?;
            drop(worker);
            authority.check()?;
            Ok(rows)
        })
        .await
        .map_err(|_| SendError::Unavailable)?
    }
}
fn check(authority: Option<&dyn AccountAuthority>) -> Result<(), SendError> {
    if let Some(authority) = authority {
        authority.check()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
