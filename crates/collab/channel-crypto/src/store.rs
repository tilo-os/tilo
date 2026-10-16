//! Where a steward keeps its group, journal and ledger.
//!
//! A [`ChannelSteward`](crate::ChannelSteward) keeps three records per
//! channel ([`StewardRecord`]) and replaces one whole record per write. Each
//! membership change writes them in this order:
//!
//! 1. the group, sealed, with the staged commit;
//! 2. the ledger;
//! 3. the journal, with the new frame and its deliveries;
//! 4. the group again, with the commit merged.
//!
//! On open, a journal ahead of the group means 3 happened, so the staged
//! commit is merged; otherwise a staged commit is discarded. That is correct
//! for any prefix of the sequence, so a store needs only two guarantees:
//!
//! - a write replaces its record whole or not at all;
//! - what a later open reads is a prefix of the writes, in order.
//!
//! [`FileStewardStore`] gets both from an atomic, fsynced rename per write.
//! [`MemoryStewardStore`] holds the records in memory for a device with no
//! private filesystem, and [`MemoryStewardStore::export`] snapshots them
//! for its own durable store. In a browser that store is `IndexedDB`: the
//! page writes each export in one readwrite transaction with strict
//! durability, so `IndexedDB` commits the whole export or none of it, and
//! commits transactions on one store in the order they were created. An
//! export taken at any point, even while a change awaits its signature, is
//! then a prefix of the sequence above, and a reloaded page opens exactly the
//! states a crash leaves on a filesystem.
//!
//! A frame leaves the steward only through
//! [`ChannelSteward::outbox`](crate::ChannelSteward::outbox) once 3 is
//! durable. A store that makes writes durable later than it returns, as an
//! exported memory store does, must therefore commit the export holding a
//! frame before the frame is sent: otherwise a reload could discard a
//! commit members already applied, and issue another at the same epoch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use collab_types::ChannelId;
use serde::{Deserialize, Serialize};

use crate::signer::MaybeSendSync;
use crate::{Error, Result};

/// Exports larger than this are refused when imported.
const MAX_EXPORT_BYTES: usize = 320 * 1024 * 1024;

/// One of a steward's records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StewardRecord {
    /// The MLS group, sealed under the vault key.
    Group,
    /// The governance journal: signed frames and their deliveries.
    Journal,
    /// Which request each member was admitted with and what it acknowledged.
    Ledger,
}

/// Durable storage for one channel's steward records; see the module docs
/// for the guarantees a store gives.
pub trait StewardStore: MaybeSendSync {
    /// The stored record, or `None` when it was never written.
    ///
    /// # Errors
    ///
    /// Returns an error when the record exists and cannot be read.
    fn read(&self, record: StewardRecord) -> Result<Option<Vec<u8>>>;

    /// Replace `record` whole.
    ///
    /// # Errors
    ///
    /// Returns an error when the write did not take; the previous record is
    /// then intact.
    fn write(&mut self, record: StewardRecord, bytes: &[u8]) -> Result<()>;
}

/// A steward's records as owner-only files in a directory:
/// `<channel>.group`, `<channel>.governance` and `<channel>.steward`.
#[derive(Clone, Debug)]
pub struct FileStewardStore {
    dir: PathBuf,
    channel: ChannelId,
}

impl FileStewardStore {
    /// The records of `channel` under `dir`.
    pub fn new(dir: &Path, channel: ChannelId) -> Self {
        Self {
            dir: dir.to_path_buf(),
            channel,
        }
    }

    fn path(&self, record: StewardRecord) -> PathBuf {
        let extension = match record {
            StewardRecord::Group => "group",
            StewardRecord::Journal => "governance",
            StewardRecord::Ledger => "steward",
        };
        self.dir.join(format!("{}.{extension}", self.channel.0))
    }
}

impl StewardStore for FileStewardStore {
    fn read(&self, record: StewardRecord) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(record)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Io(error)),
        }
    }

    fn write(&mut self, record: StewardRecord, bytes: &[u8]) -> Result<()> {
        tilo_files::native::atomic::write_private(&self.path(record), bytes).map_err(Error::Io)
    }
}

/// A steward's records in memory, exported whole for a durable store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryStewardStore {
    records: BTreeMap<StewardRecord, Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    version: u8,
    records: BTreeMap<StewardRecord, Vec<u8>>,
}

const EXPORT_VERSION: u8 = 1;

impl MemoryStewardStore {
    /// An empty store: the steward opening it creates the group.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every record, for the owner's durable store. The group record is
    /// sealed under the vault key; the journal and ledger hold no secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the records cannot be encoded.
    pub fn export(&self) -> Result<Vec<u8>> {
        postcard::to_allocvec(&Export {
            version: EXPORT_VERSION,
            records: self.records.clone(),
        })
        .map_err(|error| Error::Serialization(error.to_string()))
    }

    /// The records [`Self::export`] produced. The steward that opens them
    /// checks them as it checks files.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized, undecodable or unknown export.
    pub fn import(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_EXPORT_BYTES {
            return Err(Error::InvalidState("steward export is too large".into()));
        }
        let export: Export =
            postcard::from_bytes(bytes).map_err(|error| Error::Serialization(error.to_string()))?;
        if export.version != EXPORT_VERSION {
            return Err(Error::InvalidState(
                "unsupported steward export version".into(),
            ));
        }
        Ok(Self {
            records: export.records,
        })
    }
}

impl StewardStore for MemoryStewardStore {
    fn read(&self, record: StewardRecord) -> Result<Option<Vec<u8>>> {
        Ok(self.records.get(&record).cloned())
    }

    fn write(&mut self, record: StewardRecord, bytes: &[u8]) -> Result<()> {
        self.records.insert(record, bytes.to_vec());
        Ok(())
    }
}
