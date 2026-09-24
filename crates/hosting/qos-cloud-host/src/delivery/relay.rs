//! Original retained payloads queued for the existing relay transport.
//! Retiring a successful send is not a device consumer acknowledgement.
use super::schema::QueuedPublication;
use crate::{Store, StoredPublication};
use anyhow::{Context, Result, ensure};
use qos_relay::delivery_v3::commit::{PublicationPayload, StoreGeneration};

pub(crate) struct RelayBatch {
    incarnation: StoreGeneration,
    entries: Vec<(QueuedPublication, PublicationPayload)>,
}
impl RelayBatch {
    pub(crate) fn messages(&self) -> impl Iterator<Item = Result<StoredPublication>> + '_ {
        self.entries.iter().map(|(reference, payload)| {
            let publication = StoredPublication {
                message: postcard::from_bytes(payload.bytes())?,
                kind: reference.kind,
            };
            publication.validate()?;
            Ok(publication)
        })
    }
}
impl Store {
    pub(crate) fn delivery_relay_batch(&self) -> Result<RelayBatch> {
        self.ensure_reconciled()?;
        let delivery = self
            .state
            .delivery
            .as_ref()
            .context("delivery storage not selected")?;
        ensure!(
            self.state.version == super::VERSION,
            "delivery selection mismatch"
        );
        Ok(RelayBatch {
            incarnation: delivery.ledger.store_generation,
            entries: delivery
                .queue
                .iter()
                .take(16)
                .map(|reference| Ok((reference.clone(), delivery.pool.pin(&reference.payload)?)))
                .collect::<Result<_>>()?,
        })
    }
    pub(crate) fn retire_delivery_relay_batch(
        &mut self,
        batch: &RelayBatch,
        sent: usize,
    ) -> Result<bool> {
        self.ensure_reconciled()?;
        ensure!(
            sent <= batch.entries.len(),
            "relay send exceeds original batch"
        );
        if sent == 0 {
            return Ok(false);
        }
        let delivery = self
            .state
            .delivery
            .as_ref()
            .context("delivery storage not selected")?;
        if delivery.ledger.store_generation != batch.incarnation
            || delivery.queue.len() < sent
            || !delivery.queue[..sent]
                .iter()
                .zip(&batch.entries[..sent])
                .all(|(current, (original, _))| current == original)
        {
            return Ok(false);
        }
        let mut next = self.state.clone();
        next.delivery
            .as_mut()
            .expect("retained delivery")
            .queue
            .drain(..sent);
        self.commit(next)?;
        Ok(true)
    }
}
#[cfg(test)]
mod tests;
