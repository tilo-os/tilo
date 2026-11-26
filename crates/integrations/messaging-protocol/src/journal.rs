//! Device-owned, encrypted send journal, never a replicated room outbox.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use advisory_lock::FileLock;
use secret_vault::{VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::signal_session::{Completion, JournalError, SendJournal};
use crate::{Route, Service};

const MAX_JOURNAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPERATIONS: usize = 16_384;

pub const FORMAT: VaultFormat = VaultFormat {
    name: "tilo.messaging-send-journal.v1",
    magic: b"TMSJ",
    secret_service_label: "Tilo messaging send journal",
    secret_service_attributes: &[
        ("application", "so.tilo.messaging-broker"),
        ("purpose", "send-journal"),
    ],
    dpapi_entropy: b"tilo.messaging-send-journal.v1",
};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub actor: String,
    pub device: String,
    pub service: Service,
    pub account: String,
    pub linking_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Started,
    Submitted(crate::ProtocolReference),
    Acknowledged(crate::ProtocolReference),
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RecipientEvidence {
    Delivered,
    Read,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    route: Route,
    payload: String,
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipient_evidence: Option<RecipientEvidence>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.payload.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Log {
    version: u32,
    owner: Owner,
    operations: BTreeMap<String, Entry>,
}

/// Encryption key, payloads and account identifiers are never Debug output.
pub struct EncryptedJournal {
    path: PathBuf,
    key: VaultKey,
    log: Log,
    usable: bool,
    _lock: FileLock,
}

impl Owner {
    pub(crate) fn valid_actor(&self) -> bool {
        self.actor.len() == 64
            && self
                .actor
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

impl EncryptedJournal {
    /// The frozen authority bound to this journal, never inferred from labels.
    pub fn owner(&self) -> &Owner {
        &self.log.owner
    }

    /// Open a host-selected private directory under its supplied key/authority.
    /// An existing unreadable, corrupt or differently owned journal is never reset.
    ///
    /// # Errors
    /// Refuses competing owners, unsafe files, wrong keys/authority, oversized
    /// state and persistence failures. No secret-store or network action occurs.
    pub fn open(root: &Path, owner: &Owner, key: VaultKey) -> Result<Self, JournalError> {
        if !owner.valid_actor()
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
        {
            return Err(JournalError::Unavailable);
        }
        tilo_files::native::private::directory(root).map_err(|_| JournalError::Unavailable)?;
        let lock = tilo_files::native::private::lock_file(&root.join("owner.lock"))
            .map_err(|_| JournalError::Unavailable)?;
        let lock = FileLock::try_exclusive(lock).map_err(|_| JournalError::Unavailable)?;
        let path = root.join(secret_vault::VAULT_FILE);
        let original = read_log(&path, &key)?;
        let mut changed = original.is_none();
        let mut log = original.unwrap_or_else(|| Log {
            version: 1,
            owner: owner.clone(),
            operations: BTreeMap::new(),
        });
        if log.version != 1 || &log.owner != owner || log.operations.len() > MAX_OPERATIONS {
            return Err(JournalError::Unavailable);
        }
        for entry in log.operations.values_mut() {
            if entry.route.service() != owner.service
                || entry.route.account() != owner.account
                || entry.payload.len() > crate::signal::MAX_FRAME_BYTES
            {
                return Err(JournalError::Unavailable);
            }
            if let Status::Acknowledged(reference) | Status::Submitted(reference) = &entry.status
                && !reference.valid_for(owner.service)
            {
                return Err(JournalError::Unavailable);
            }
            if entry.status == Status::Started {
                entry.status = Status::Uncertain;
                changed = true;
            }
        }
        let mut journal = Self {
            path,
            key,
            log,
            usable: true,
            _lock: lock,
        };
        if changed {
            journal.persist()?;
        }
        Ok(journal)
    }

    pub fn status(&self, operation: &str) -> Option<Status> {
        self.log
            .operations
            .get(operation)
            .map(|entry| entry.status.clone())
    }

    #[cfg(all(unix, feature = "signal-broker"))]
    pub(crate) fn signal_operations(
        &self,
        route: &Route,
        timestamps: &[u64],
    ) -> Result<Vec<String>, JournalError> {
        if !self.usable
            || self.log.owner.service != Service::Signal
            || timestamps.is_empty()
            || timestamps.len() > 512
            || timestamps.contains(&0)
        {
            return Err(JournalError::Unavailable);
        }
        let mut matched: BTreeMap<u64, Option<String>> =
            timestamps.iter().map(|time| (*time, None)).collect();
        for (operation, entry) in &self.log.operations {
            if &entry.route != route {
                continue;
            }
            let Status::Acknowledged(crate::ProtocolReference::Signal { timestamp }) =
                &entry.status
            else {
                continue;
            };
            if let Some(slot) = matched.get_mut(timestamp) {
                if slot.is_some() {
                    return Err(JournalError::Unavailable);
                }
                *slot = Some(operation.clone());
            }
        }
        matched
            .into_values()
            .collect::<Option<Vec<_>>>()
            .ok_or(JournalError::Unavailable)
    }

    pub fn recipient_evidence(&self, operation: &str) -> Option<RecipientEvidence> {
        if !self.usable {
            return None;
        }
        self.log.operations.get(operation)?.recipient_evidence
    }

    pub fn check_observation(&self, operation: &str, route: &Route) -> Result<(), JournalError> {
        if !self.usable
            || self
                .log
                .operations
                .get(operation)
                .is_none_or(|entry| &entry.route != route)
        {
            return Err(JournalError::Unavailable);
        }
        Ok(())
    }

    /// Persist monotonic evidence for the exact original operation and route.
    /// This never resubmits an uncertain operation or promotes a server ack.
    pub fn observe_recipient(
        &mut self,
        operation: &str,
        route: &Route,
        evidence: RecipientEvidence,
    ) -> Result<(), JournalError> {
        self.check_observation(operation, route)?;
        let entry = self
            .log
            .operations
            .get_mut(operation)
            .ok_or(JournalError::Unavailable)?;
        if entry
            .recipient_evidence
            .is_some_and(|previous| previous >= evidence)
        {
            return Ok(());
        }
        entry.recipient_evidence = Some(evidence);
        self.persist()
    }

    /// Exact frozen bytes for host reconciliation, never a new submission.
    pub fn payload(&self, operation: &str) -> Option<Zeroizing<Vec<u8>>> {
        self.log
            .operations
            .get(operation)
            .map(|entry| Zeroizing::new(entry.payload.as_bytes().to_vec()))
    }

    fn persist(&mut self) -> Result<(), JournalError> {
        let result = self.write_snapshot();
        if result.is_err() {
            self.usable = false;
        }
        result
    }

    fn write_snapshot(&self) -> Result<(), JournalError> {
        let encoded =
            Zeroizing::new(serde_json::to_vec(&self.log).map_err(|_| JournalError::Unavailable)?);
        if encoded.len() > MAX_JOURNAL_BYTES {
            return Err(JournalError::Unavailable);
        }
        let sealed = FORMAT
            .seal(&self.key, &encoded)
            .ok_or(JournalError::Unavailable)?;
        tilo_files::native::private::write_atomic(&self.path, &sealed)
            .map_err(|_| JournalError::Unavailable)
    }
}

impl SendJournal for EncryptedJournal {
    fn begin(
        &mut self,
        operation: &str,
        route: &Route,
        payload: &[u8],
    ) -> Result<(), JournalError> {
        if !self.usable
            || !crate::identifier(operation)
            || route.account() != self.log.owner.account
            || route.service() != self.log.owner.service
            || payload.len() > crate::signal::MAX_FRAME_BYTES
            || self.log.operations.len() >= MAX_OPERATIONS
        {
            return Err(JournalError::Unavailable);
        }
        if self.log.operations.contains_key(operation) {
            return Err(JournalError::AlreadyReserved);
        }
        let payload = std::str::from_utf8(payload)
            .map_err(|_| JournalError::Unavailable)?
            .to_owned();
        self.log.operations.insert(
            operation.into(),
            Entry {
                route: route.clone(),
                payload,
                status: Status::Started,
                recipient_evidence: None,
            },
        );
        self.persist()
    }

    fn complete(&mut self, operation: &str, result: Completion) -> Result<(), JournalError> {
        if !self.usable {
            return Err(JournalError::Unavailable);
        }
        if let Completion::Acknowledged(reference) | Completion::Submitted(reference) = &result
            && !reference.valid_for(self.log.owner.service)
        {
            return Err(JournalError::Unavailable);
        }
        let entry = self
            .log
            .operations
            .get_mut(operation)
            .ok_or(JournalError::Unavailable)?;
        if entry.status != Status::Started {
            return Err(JournalError::AlreadyReserved);
        }
        entry.status = match result {
            Completion::Submitted(reference) => Status::Submitted(reference),
            Completion::Acknowledged(reference) => Status::Acknowledged(reference),
            Completion::Uncertain => Status::Uncertain,
        };
        self.persist()
    }
}

fn read_log(path: &Path, key: &VaultKey) -> Result<Option<Log>, JournalError> {
    let file = match tilo_files::native::private::open_owned_for_read(
        path,
        (MAX_JOURNAL_BYTES + 1024) as u64,
    ) {
        Ok((file, _)) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(JournalError::Unavailable),
    };
    let mut sealed = Vec::new();
    file.take((MAX_JOURNAL_BYTES + 1025) as u64)
        .read_to_end(&mut sealed)
        .map_err(|_| JournalError::Unavailable)?;
    if sealed.len() > MAX_JOURNAL_BYTES + 1024 {
        return Err(JournalError::Unavailable);
    }
    let plain = FORMAT.open(key, &sealed).ok_or(JournalError::Unavailable)?;
    serde_json::from_slice(&plain)
        .map(Some)
        .map_err(|_| JournalError::Unavailable)
}
