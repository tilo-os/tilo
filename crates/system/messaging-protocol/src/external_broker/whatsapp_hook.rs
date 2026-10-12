//! SDK durability bridge, installed before connecting an already linked account.
use super::{Arc, ExternalBroker, Owner, SendError};
use std::sync::{OnceLock, Weak};
use whatsapp_rust::types::durability_hook::{InboundDurabilityHook, InboundMessage};

/// Install in the SDK builder, then bind the assembled account supervisor before
/// connecting. The SDK verifies its backend's pending-inbound support at build.
/// This creates no credentials/grants and owns no network or plaintext storage.
/// A weak supervisor binding prevents a client/hook/broker ownership cycle;
/// losing the supervisor fails the hook rather than acknowledging an event.
pub struct WhatsappInboxHook {
    owner: Owner,
    bound: OnceLock<Weak<ExternalBroker>>,
    groups: OnceLock<Weak<crate::supervisor::AccountSupervisor>>,
}
impl WhatsappInboxHook {
    /// # Errors
    /// Refuses malformed or non-WhatsApp account ownership.
    pub fn new(owner: Owner) -> Result<Self, SendError> {
        if owner.service != crate::Service::Whatsapp
            || !owner.valid_principal()
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
            || owner.linking_generation == 0
        {
            return Err(SendError::InvalidRequest);
        }
        Ok(Self {
            owner,
            bound: OnceLock::new(),
            groups: OnceLock::new(),
        })
    }
    pub(crate) fn bind_groups(
        &self,
        supervisor: &Arc<crate::supervisor::AccountSupervisor>,
    ) -> Result<(), SendError> {
        self.groups
            .set(Arc::downgrade(supervisor))
            .map_err(|_| SendError::AlreadyReserved)
    }
    /// Bind once to this exact account and SDK, on the supervisor's worker.
    /// The supervisor must keep its Arc alive while receiving and retain current
    /// grant custody. Relinking requires a new hook/client/broker assembly.
    /// # Errors
    /// Refuses wrong account/link/source and any attempt to replace a binding.
    pub async fn bind(
        &self,
        broker: &Arc<ExternalBroker>,
        source: &Arc<whatsapp_rust::Client>,
    ) -> Result<(), SendError> {
        if broker.owner != self.owner {
            return Err(SendError::Revoked);
        }
        let worker = broker.worker.lock().await;
        let matched = worker.adapter.whatsapp_source(source);
        drop(worker);
        if !matched {
            return Err(SendError::Revoked);
        }
        self.bound
            .set(Arc::downgrade(broker))
            .map_err(|_| SendError::AlreadyReserved)
    }
}
#[whatsapp_rust::async_trait]
impl InboundDurabilityHook for WhatsappInboxHook {
    async fn on_messages(
        &self,
        client: Arc<whatsapp_rust::Client>,
        batch: &[InboundMessage],
    ) -> whatsapp_rust::anyhow::Result<()> {
        // Bound before cloning: a batch limit must not become an allocation limit
        // only after the SDK's body/metadata have already been copied again.
        if batch.is_empty() || batch.len() > 64 {
            return Err(whatsapp_rust::anyhow::anyhow!(
                "private inbox batch unavailable"
            ));
        }
        let broker = self.bound.get().and_then(Weak::upgrade).ok_or_else(|| {
            whatsapp_rust::anyhow::anyhow!("private inbox supervisor unavailable")
        })?;
        if batch.iter().any(|event| event.info.source.is_group) {
            let supervisor = self.groups.get().and_then(Weak::upgrade).ok_or_else(|| {
                whatsapp_rust::anyhow::anyhow!("private group supervisor unavailable")
            })?;
            let host = supervisor
                .whatsapp_group_host(&tilo_messaging::ServiceAccount::new(
                    "whatsapp",
                    &self.owner.account,
                ))
                .ok_or_else(|| {
                    whatsapp_rust::anyhow::anyhow!("private group authority unavailable")
                })?;
            host.receive(client, batch).await.map_err(|_| {
                whatsapp_rust::anyhow::anyhow!("private group inbox admission unavailable")
            })?;
            return Ok(());
        }
        broker
            .observe_whatsapp_batch(client, batch.to_vec())
            .await
            .map_err(|_| whatsapp_rust::anyhow::anyhow!("private inbox admission unavailable"))?;
        Ok(())
    }
}
