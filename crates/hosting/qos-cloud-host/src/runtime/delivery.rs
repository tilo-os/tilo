//! One tick of the existing relay loop, retaining the selected Store payload owner.
use super::*;

pub(super) enum RelayTickBatch {
    Legacy(Vec<StoredPublication>),
    Delivery(crate::delivery::relay::RelayBatch),
}
impl RelayTickBatch {
    pub(super) fn prepare(store: &mut Store) -> Result<Self> {
        if store.state.version == crate::delivery::VERSION {
            return Ok(Self::Delivery(store.delivery_relay_batch()?));
        }
        let compact = checkpoint_outbox(&store.state.outbox);
        if compact.len() != store.state.outbox.len() {
            let mut state = store.state.clone();
            state.outbox = compact;
            store.commit(state)?;
        }
        Ok(Self::Legacy(store.state.outbox.clone()))
    }
    pub(super) fn publish(
        &self,
        mut send: impl FnMut(&StoredPublication) -> Result<()>,
    ) -> Result<usize> {
        match self {
            Self::Legacy(messages) => publish_batch(messages, send),
            Self::Delivery(batch) => {
                let mut sent = 0;
                // The batch retains at most 16 original payload handles. Decode
                // one for the existing relay sender; never clone the durable pool.
                for message in batch.messages() {
                    let message = message?;
                    if publish_batch(std::slice::from_ref(&message), &mut send)? == 0 {
                        break;
                    }
                    sent += 1;
                }
                Ok(sent)
            }
        }
    }
    pub(super) fn retire(&self, store: &mut Store, sent: usize) -> Result<()> {
        if sent == 0 {
            return Ok(());
        }
        match self {
            Self::Legacy(batch) => {
                anyhow::ensure!(sent <= batch.len(), "relay send exceeds original batch");
                let mut state = store.state.clone();
                if state.delivery.is_none()
                    && state.outbox.len() >= sent
                    && postcard::to_stdvec(&state.outbox[..sent])?
                        == postcard::to_stdvec(&batch[..sent])?
                {
                    state.outbox.drain(..sent);
                    store.commit(state)?;
                }
            }
            Self::Delivery(batch) => {
                store.retire_delivery_relay_batch(batch, sent)?;
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
