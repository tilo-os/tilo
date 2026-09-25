//! Operationless initial baseline history. Serialized evidence grants no authority.
use super::{ConnectionId, ConnectionStore, Row, StoreError, format};
use sha2::{Digest, Sha256};

impl format::InitialEmptyBaselineV1 {
    pub(in crate::connection) fn from_review(
        record: &super::ConnectionRecord,
        preview: [u8; 32],
    ) -> Self {
        Self {
            version: 1,
            connection: record.id.0,
            revision: record.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            preview,
        }
    }
}
fn original(connection: &format::Connection) -> format::Connection {
    let mut initial = connection.clone();
    initial.initial_empty = None;
    initial.directory_chain = None;
    initial.directory_chain_schedule = None;
    initial.revision = 1;
    initial.lifecycle = 0;
    initial.baseline = None;
    initial.baseline_root = None;
    initial.baseline_count = 0;
    initial.fence = 0;
    initial.serial = 0;
    initial.pending = None;
    initial
}
fn preview_digest(initial: &format::Connection) -> Result<[u8; 32], StoreError> {
    let mut hash = Sha256::new();
    hash.update(b"Connected explicit initial preview v1");
    hash.update(format::encode(initial)?);
    for side in [0_u8, 1] {
        hash.update([side]);
        hash.update(0_u64.to_le_bytes());
    }
    Ok(hash.finalize().into())
}
fn validate_fact(
    fact: &format::InitialEmptyBaselineV1,
    connection: &format::Connection,
) -> Result<(), StoreError> {
    if fact.version != 1
        || fact.connection != connection.id
        || fact.revision != 1
        || fact.source != connection.source.identity
        || fact.destination != connection.destination.identity
        || fact.preview != preview_digest(&original(connection))?
    {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}
pub(super) fn validate_history(connection: &format::Connection) -> Result<(), StoreError> {
    if let Some(fact) = &connection.initial_empty {
        validate_fact(fact, connection)?;
        if connection.revision < 2
            || connection
                .baseline
                .is_none_or(|revision| revision < 2 || revision > connection.revision)
            || (connection.serial == 0
                && (connection.baseline != Some(2)
                    || connection.baseline_root.is_some()
                    || connection.baseline_count != 0
                    || connection.pending.is_some()
                    || connection.directory_chain.is_some()
                    || connection.directory_chain_schedule.is_some()
                    // With no operation, only the one-way disconnect reducers
                    // can advance the initial commit's revision and fence.
                    || !matches!(
                        (connection.lifecycle, connection.revision, connection.fence),
                        (0, 2, 0) | (1, 3, 1) | (2, 4, 1)
                    )))
        {
            return Err(StoreError::Corrupt);
        }
    } else if connection.serial == 0 && connection.baseline.is_some() {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}
impl ConnectionStore {
    pub(super) fn reduce_initial_empty(
        &self,
        fact: &format::InitialEmptyBaselineV1,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(ConnectionId(fact.connection))?;
        validate_fact(fact, &connection)?;
        if connection != original(&connection) || connection.revision != fact.revision {
            return Err(StoreError::StaleRevision);
        }
        connection.initial_empty = Some(fact.clone());
        connection.revision = format::next(connection.revision)?;
        connection.baseline = Some(connection.revision);
        connection.decode()?;
        Ok(vec![Row::Connection(connection)])
    }
    #[cfg(target_os = "linux")]
    pub(in crate::connection) fn operationless_baseline(
        &self,
        id: ConnectionId,
    ) -> Result<(), StoreError> {
        let connection = self.connection(id)?;
        validate_history(&connection)?;
        if connection.initial_empty.is_none()
            || connection.serial != 0
            || connection.pending.is_some()
            || connection.lifecycle != 0
            || connection.baseline != Some(2)
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(in crate::connection) fn commit_initial_empty(
        &mut self,
        proof: &mut super::super::transaction::InitialEmptyProof<'_>,
    ) -> Result<super::DurableAck<super::Revision>, StoreError> {
        self.guard()?;
        let fact = proof.fact(self)?;
        let revision = format::next(fact.revision)?;
        self.append_checked_with_baseline(
            super::Change::InitialEmptyBaseline { fact },
            super::RecoveryChecks::default(),
            super::BaselineAppendCheck::InitialEmpty(proof),
        )?;
        Ok(self.ack(revision))
    }
}
