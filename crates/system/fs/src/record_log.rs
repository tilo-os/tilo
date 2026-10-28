//! The durable operation log behind the record repository.
//!
//! Every accepted signed envelope is appended to one redb table under a
//! global, strictly increasing position; records, stream cursors and the
//! history projection are rebuilt from it in that order. A signed changeset
//! is stored beside its envelopes in the same write transaction, so after a
//! crash either all of a changeset is present or none of it (redb commits
//! are atomic and durable when `commit` returns).
//!
//! Envelopes are stored as their canonical wire bytes and verified once, at
//! ingest; reopening trusts the log (`storage.md` §8.1).

mod external_reservation;
pub(crate) use external_reservation::{
    ExternalCanonicalIds, ExternalReservationInput, PreviousCanonicalIds,
};

use std::path::Path;
use std::sync::Mutex;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use uuid::Uuid;

use crate::record_changeset::SignedChangesetV1;
use crate::record_replication::RecordEnvelopeV1;

/// Position → workspace id (16 bytes) followed by the canonical envelope.
const OPS: TableDefinition<u64, &[u8]> = TableDefinition::new("record_ops");
/// Workspace (16) ‖ device (32) ‖ sequence (8, big-endian) → the canonical
/// signed changeset, then the position of its first envelope (8 bytes).
const CHANGESETS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("record_changesets");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("record_log_meta");

const SCHEMA_KEY: &str = "schema";
pub const RECORD_LOG_SCHEMA: u16 = 1;

/// One stored changeset and where its envelopes start in the log.
pub(crate) struct LoggedChangeset {
    pub changeset: SignedChangesetV1,
    pub first_position: u64,
}

pub(crate) struct RecordLog {
    db: Database,
    /// The position the next appended envelope gets.
    next: Mutex<u64>,
    /// Tests make appends fail with this.
    #[cfg(test)]
    pub fail_appends: std::sync::atomic::AtomicBool,
}

fn changeset_key(changeset: &SignedChangesetV1) -> Vec<u8> {
    let mut key = Vec::with_capacity(56);
    key.extend_from_slice(changeset.header.workspace_id.as_bytes());
    key.extend_from_slice(changeset.header.actor.device.as_bytes());
    key.extend_from_slice(&changeset.header.sequence.to_be_bytes());
    key
}

/// Stored op encodings: the canonical wire bytes, or JSON for the few
/// operations whose types the canonical format can write but not read back
/// (internally tagged enums inside checked-restore transactions).
const OP_CANONICAL: u8 = 0;
const OP_JSON: u8 = 1;

fn encode_op(workspace_id: Uuid, envelope: &RecordEnvelopeV1) -> Result<Vec<u8>, RecordLogError> {
    let canonical = envelope.canonical_bytes()?;
    let (format, bytes) =
        if collab_types::canonical::from_canonical_bytes::<RecordEnvelopeV1>(&canonical).is_ok() {
            (OP_CANONICAL, canonical)
        } else {
            (
                OP_JSON,
                serde_json::to_vec(envelope).map_err(|_| RecordLogError::Corrupt)?,
            )
        };
    let mut value = Vec::with_capacity(bytes.len().saturating_add(17));
    value.extend_from_slice(workspace_id.as_bytes());
    value.push(format);
    value.extend_from_slice(&bytes);
    Ok(value)
}

fn decode_op(value: &[u8]) -> Result<(Uuid, RecordEnvelopeV1), RecordLogError> {
    let (workspace, rest) = value.split_at_checked(16).ok_or(RecordLogError::Corrupt)?;
    let workspace = Uuid::from_slice(workspace).map_err(|_| RecordLogError::Corrupt)?;
    let (format, envelope) = rest.split_first().ok_or(RecordLogError::Corrupt)?;
    let envelope = match *format {
        OP_CANONICAL => collab_types::canonical::from_canonical_bytes(envelope)?,
        OP_JSON => serde_json::from_slice(envelope).map_err(|_| RecordLogError::Corrupt)?,
        _ => return Err(RecordLogError::Corrupt),
    };
    Ok((workspace, envelope))
}

impl RecordLog {
    /// Open or create the log at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or was written
    /// by a newer schema.
    pub fn open(path: &Path) -> Result<Self, RecordLogError> {
        let db = Database::create(path)?;
        let txn = db.begin_write()?;
        let next = {
            let ops = txn.open_table(OPS)?;
            txn.open_table(CHANGESETS)?;
            let mut meta = txn.open_table(META)?;
            let stored = meta
                .get(SCHEMA_KEY)?
                .map(|value| {
                    <[u8; 2]>::try_from(value.value())
                        .map(u16::from_be_bytes)
                        .map_err(|_| RecordLogError::Corrupt)
                })
                .transpose()?;
            match stored {
                Some(schema) if schema != RECORD_LOG_SCHEMA => {
                    return Err(RecordLogError::UnsupportedSchema(schema));
                }
                Some(_) => {}
                None => {
                    meta.insert(SCHEMA_KEY, RECORD_LOG_SCHEMA.to_be_bytes().as_slice())?;
                }
            }
            ops.last()?
                .map_or(1, |(position, _)| position.value().saturating_add(1))
        };
        txn.commit()?;
        Ok(Self {
            db,
            next: Mutex::new(next),
            #[cfg(test)]
            fail_appends: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Append `ops` in order, and `changeset` beside them, in one durable
    /// transaction. Returns the position of the
    /// first op; op `i` is at `first + i`.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding fails or the transaction does not
    /// commit; nothing is then written.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "positions are handed out only by a transaction that commits"
    )]
    pub fn append(
        &self,
        ops: &[(Uuid, &RecordEnvelopeV1)],
        changeset: Option<&SignedChangesetV1>,
    ) -> Result<u64, RecordLogError> {
        #[cfg(test)]
        if self.fail_appends.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(RecordLogError::Database(redb::Error::Io(
                std::io::Error::other("injected record log write failure"),
            )));
        }
        let mut next = self.next.lock().map_err(|_| RecordLogError::Poisoned)?;
        let first = *next;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(OPS)?;
            for (offset, (workspace_id, envelope)) in ops.iter().enumerate() {
                let position = first.saturating_add(offset as u64);
                table.insert(position, encode_op(*workspace_id, envelope)?.as_slice())?;
            }
            if let Some(changeset) = changeset {
                let mut value = changeset.canonical_bytes()?;
                value.extend_from_slice(&first.to_be_bytes());
                txn.open_table(CHANGESETS)?
                    .insert(changeset_key(changeset).as_slice(), value.as_slice())?;
            }
        }
        txn.commit()?;
        *next = first.saturating_add(ops.len() as u64);
        Ok(first)
    }

    /// Every op in log order.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable or undecodable entries; `visit`'s
    /// own errors stop the replay.
    pub fn replay_ops<E: From<RecordLogError>>(
        &self,
        mut visit: impl FnMut(u64, Uuid, RecordEnvelopeV1) -> Result<(), E>,
    ) -> Result<(), E> {
        let txn = self.db.begin_read().map_err(RecordLogError::from)?;
        let table = txn.open_table(OPS).map_err(RecordLogError::from)?;
        for row in table.iter().map_err(RecordLogError::from)? {
            let (position, value) = row.map_err(RecordLogError::from)?;
            let (workspace_id, envelope) = decode_op(value.value())?;
            visit(position.value(), workspace_id, envelope)?;
        }
        Ok(())
    }

    /// Every stored changeset, by workspace, device and sequence.
    pub fn changesets(&self) -> Result<Vec<LoggedChangeset>, RecordLogError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CHANGESETS)?;
        let mut out = Vec::with_capacity(usize::try_from(table.len()?).unwrap_or(0));
        for row in table.iter()? {
            let (_, value) = row?;
            let value = value.value();
            let split = value.len().checked_sub(8).ok_or(RecordLogError::Corrupt)?;
            let (changeset, position) = value.split_at(split);
            let first_position = <[u8; 8]>::try_from(position)
                .map(u64::from_be_bytes)
                .map_err(|_| RecordLogError::Corrupt)?;
            out.push(LoggedChangeset {
                changeset: SignedChangesetV1::from_canonical_bytes(changeset)?,
                first_position,
            });
        }
        Ok(out)
    }

    /// Number of ops in the log.
    ///
    /// # Errors
    ///
    /// Returns an error when the log cannot be read.
    pub fn op_count(&self) -> Result<u64, RecordLogError> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(OPS)?.len()?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecordLogError {
    #[error("record log database failed: {0}")]
    Database(#[from] redb::Error),
    #[error("record log entry is malformed")]
    Corrupt,
    #[error("record log was written by unsupported schema {0}")]
    UnsupportedSchema(u16),
    #[error("record log writer lock is poisoned")]
    Poisoned,
    #[error("record log entry encoding failed: {0}")]
    Canonical(#[from] collab_types::CanonicalError),
}

macro_rules! redb_error {
    ($($t:ty),*) => {
        $(impl From<$t> for RecordLogError {
            fn from(error: $t) -> Self {
                Self::Database(error.into())
            }
        })*
    };
}
redb_error!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);
