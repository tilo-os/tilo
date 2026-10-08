//! Device-owned, encrypted send journal, never a replicated room outbox.
//!
//! The broker supplies its secret-service key and certified device/account
//! context. No keys are generated here and no local-key fallback is offered.
//! One owner holds the lock for the journal's lifetime. Interrupted reservations
//! become uncertain on restart; operation IDs are never evicted or reused.

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

/// Stable persisted encryption domain; do not rename an existing format.
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

/// Authority is supplied and verified by the host, not inferred from contacts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    /// Certified person root supplied by the authenticated host.
    pub principal: String,
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

/// Durable recipient evidence is independent of submission/server acknowledgment.
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
struct Ledger {
    version: u32,
    owner: Owner,
    operations: BTreeMap<String, Entry>,
}

/// Encryption key, payloads and account identifiers are never Debug output.
pub struct EncryptedJournal {
    path: PathBuf,
    key: VaultKey,
    ledger: Ledger,
    usable: bool,
    _lock: FileLock,
}

impl Owner {
    pub(crate) fn valid_principal(&self) -> bool {
        self.principal.len() == 64
            && self
                .principal
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

impl EncryptedJournal {
    /// Open under an already provisioned OS-keychain key. This runtime path
    /// never creates a key, migrates a local key or weakens custody when locked.
    /// Provisioning belongs to the separately authorized linking workflow.
    ///
    /// # Errors
    /// Refuses absent/locked/non-OS custody or any journal ownership failure.
    pub fn open_system(root: &Path, owner: &Owner) -> Result<Self, JournalError> {
        let store = tilo_secrets::SecretStore::for_app("so.tilo.messaging-broker");
        if store.protection() != tilo_secrets::Protection::OsKeychain {
            return Err(JournalError::Unavailable);
        }
        Self::open_existing_in(root, owner, &store)
    }

    /// Host-injected custody, for hosts supplying their own existing store.
    /// No missing key is created, including for a new journal directory.
    ///
    /// # Errors
    /// Refuses absent/unreadable keys and unsafe or differently owned journals.
    pub fn open_existing_in(
        root: &Path,
        owner: &Owner,
        store: &tilo_secrets::SecretStore,
    ) -> Result<Self, JournalError> {
        if store.app_id() != "so.tilo.messaging-broker" {
            return Err(JournalError::Unavailable);
        }
        // true requests lookup only: secret-vault must not mint a new key.
        let key = FORMAT
            .load_or_create_system_key_in(true, store)
            .ok_or(JournalError::Unavailable)?;
        Self::open(root, owner, key)
    }

    /// The frozen authority bound to this journal, never inferred from labels.
    pub fn owner(&self) -> &Owner {
        &self.ledger.owner
    }

    /// Open a host-selected private directory under its supplied key/authority.
    /// An existing unreadable, corrupt or differently owned journal is never reset.
    ///
    /// # Errors
    /// Refuses competing owners, unsafe files, wrong keys/authority, oversized
    /// state and persistence failures. No secret-store or network action occurs.
    pub fn open(root: &Path, owner: &Owner, key: VaultKey) -> Result<Self, JournalError> {
        if !owner.valid_principal()
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
        {
            return Err(JournalError::Unavailable);
        }
        private_fs::directory(root).map_err(|_| JournalError::Unavailable)?;
        let lock = private_fs::lock_file(&root.join("owner.lock"))
            .map_err(|_| JournalError::Unavailable)?;
        let lock = FileLock::try_exclusive(lock).map_err(|_| JournalError::Unavailable)?;
        let path = root.join(secret_vault::VAULT_FILE);
        let original = read_ledger(&path, &key)?;
        let mut changed = original.is_none();
        let mut ledger = original.unwrap_or_else(|| Ledger {
            version: 1,
            owner: owner.clone(),
            operations: BTreeMap::new(),
        });
        if ledger.version != 1 || &ledger.owner != owner || ledger.operations.len() > MAX_OPERATIONS
        {
            return Err(JournalError::Unavailable);
        }
        for entry in ledger.operations.values_mut() {
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
            ledger,
            usable: true,
            _lock: lock,
        };
        if changed {
            journal.persist()?;
        }
        Ok(journal)
    }

    /// Read protocol evidence; no status implies recipient delivery.
    pub fn status(&self, operation: &str) -> Option<Status> {
        self.ledger
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
            || self.ledger.owner.service != Service::Signal
            || timestamps.is_empty()
            || timestamps.len() > 512
            || timestamps.contains(&0)
        {
            return Err(JournalError::Unavailable);
        }
        let mut matched: BTreeMap<u64, Option<String>> =
            timestamps.iter().map(|time| (*time, None)).collect();
        for (operation, entry) in &self.ledger.operations {
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

    /// Recipient evidence is visible only after successful durable persistence.
    pub fn recipient_evidence(&self, operation: &str) -> Option<RecipientEvidence> {
        if !self.usable {
            return None;
        }
        self.ledger.operations.get(operation)?.recipient_evidence
    }

    /// Verify an observation against the original immutable journal route.
    ///
    /// # Errors
    /// Refuses unknown operations, other conversations and unavailable journals.
    pub fn check_observation(&self, operation: &str, route: &Route) -> Result<(), JournalError> {
        if !self.usable
            || self
                .ledger
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
    ///
    /// # Errors
    /// Refuses unrelated observations or a persistence failure.
    pub fn observe_recipient(
        &mut self,
        operation: &str,
        route: &Route,
        evidence: RecipientEvidence,
    ) -> Result<(), JournalError> {
        self.check_observation(operation, route)?;
        let entry = self
            .ledger
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
        self.ledger
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
        let encoded = Zeroizing::new(
            serde_json::to_vec(&self.ledger).map_err(|_| JournalError::Unavailable)?,
        );
        if encoded.len() > MAX_JOURNAL_BYTES {
            return Err(JournalError::Unavailable);
        }
        let sealed = FORMAT
            .seal(&self.key, &encoded)
            .ok_or(JournalError::Unavailable)?;
        private_fs::write_atomic(&self.path, &sealed).map_err(|_| JournalError::Unavailable)
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
            || route.account() != self.ledger.owner.account
            || route.service() != self.ledger.owner.service
            || payload.len() > crate::signal::MAX_FRAME_BYTES
            || self.ledger.operations.len() >= MAX_OPERATIONS
        {
            return Err(JournalError::Unavailable);
        }
        if self.ledger.operations.contains_key(operation) {
            return Err(JournalError::AlreadyReserved);
        }
        let payload = std::str::from_utf8(payload)
            .map_err(|_| JournalError::Unavailable)?
            .to_owned();
        self.ledger.operations.insert(
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
            && !reference.valid_for(self.ledger.owner.service)
        {
            return Err(JournalError::Unavailable);
        }
        let entry = self
            .ledger
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

fn read_ledger(path: &Path, key: &VaultKey) -> Result<Option<Ledger>, JournalError> {
    let file = match private_fs::open_owned_for_read(path, (MAX_JOURNAL_BYTES + 1024) as u64) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_receipt_persistence_never_exposes_optimistic_delivery() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        std::fs::remove_file(&journal.path).unwrap();
        std::fs::create_dir(&journal.path).unwrap();
        assert_eq!(
            journal.observe_recipient("op", &route(), RecipientEvidence::Delivered),
            Err(JournalError::Unavailable)
        );
        assert_eq!(journal.recipient_evidence("op"), None);
        assert_eq!(
            journal.check_observation("op", &route()),
            Err(JournalError::Unavailable)
        );
    }

    fn owner() -> Owner {
        Owner {
            principal: "11".repeat(32),
            device: "device-a".into(),
            service: Service::Signal,
            account: "account-a".into(),
            linking_generation: 1,
        }
    }
    fn key() -> VaultKey {
        VaultKey::from_slice(&[7; 32]).unwrap()
    }
    fn route() -> Route {
        Route::new(Service::Signal, "account-a".into(), "recipient".into()).unwrap()
    }

    #[test]
    fn reservations_survive_restart_encrypted_and_cannot_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal
            .begin("op", &route(), b"private-message-body")
            .unwrap();
        assert_eq!(journal.status("op"), Some(Status::Started));
        assert!(EncryptedJournal::open(root.path(), &owner(), key()).is_err());
        drop(journal);
        let raw = std::fs::read(root.path().join(secret_vault::VAULT_FILE)).unwrap();
        assert!(
            !raw.windows(b"private-message-body".len())
                .any(|window| window == b"private-message-body")
        );
        let mut reopened = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        assert_eq!(reopened.status("op"), Some(Status::Uncertain));
        assert_eq!(
            reopened.payload("op").unwrap().as_slice(),
            b"private-message-body"
        );
        assert_eq!(
            reopened.begin("op", &route(), b"different"),
            Err(JournalError::AlreadyReserved)
        );
    }

    #[cfg(all(unix, feature = "signal-broker"))]
    #[test]
    fn signal_receipt_correlation_refuses_unknown_foreign_and_ambiguous_operations() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("first", &route(), b"frozen first").unwrap();
        journal
            .complete(
                "first",
                Completion::Acknowledged(crate::ProtocolReference::Signal { timestamp: 43 }),
            )
            .unwrap();
        assert_eq!(
            journal.signal_operations(&route(), &[43]).unwrap(),
            vec!["first"]
        );
        assert!(journal.signal_operations(&route(), &[44]).is_err());
        let foreign = Route::new(
            Service::Signal,
            "account-a".into(),
            "other-recipient".into(),
        )
        .unwrap();
        assert!(journal.signal_operations(&foreign, &[43]).is_err());
        journal.begin("second", &route(), b"frozen second").unwrap();
        journal
            .complete(
                "second",
                Completion::Acknowledged(crate::ProtocolReference::Signal { timestamp: 43 }),
            )
            .unwrap();
        assert!(
            journal.signal_operations(&route(), &[43]).is_err(),
            "a timestamp collision cannot choose an operation"
        );
        assert_eq!(journal.recipient_evidence("first"), None);
        assert_eq!(journal.recipient_evidence("second"), None);
    }

    #[test]
    fn wrong_keys_and_authority_cannot_reset_existing_operations() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        drop(journal);
        assert!(
            EncryptedJournal::open(
                root.path(),
                &owner(),
                VaultKey::from_slice(&[8; 32]).unwrap()
            )
            .is_err()
        );
        for altered in [
            Owner {
                principal: "22".repeat(32),
                ..owner()
            },
            Owner {
                device: "device-b".into(),
                ..owner()
            },
            Owner {
                account: "account-b".into(),
                ..owner()
            },
            Owner {
                linking_generation: 2,
                ..owner()
            },
        ] {
            assert!(EncryptedJournal::open(root.path(), &altered, key()).is_err());
        }
        let journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        assert_eq!(journal.status("op"), Some(Status::Uncertain));
    }

    #[test]
    fn journals_without_a_certified_person_root_are_refused_without_reset() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        let mut legacy = serde_json::to_value(&journal.ledger).unwrap();
        legacy["owner"].as_object_mut().unwrap().remove("principal");
        drop(journal);
        let encoded = Zeroizing::new(serde_json::to_vec(&legacy).unwrap());
        let sealed = FORMAT.seal(&key(), &encoded).unwrap();
        let path = root.path().join(secret_vault::VAULT_FILE);
        std::fs::write(&path, &sealed).unwrap();
        assert!(EncryptedJournal::open(root.path(), &owner(), key()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), sealed);
    }

    #[test]
    fn malformed_principal_is_refused_before_creating_state() {
        let root = tempfile::tempdir().unwrap();
        let mut invalid = owner();
        for principal in [String::new(), "Ada".to_owned(), "AA".repeat(32)] {
            invalid.principal = principal;
            assert!(EncryptedJournal::open(root.path(), &invalid, key()).is_err());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn acknowledgment_is_durable_and_cannot_be_overwritten_by_stale_callbacks() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        journal
            .complete(
                "op",
                Completion::Acknowledged(crate::ProtocolReference::Signal { timestamp: 42 }),
            )
            .unwrap();
        assert_eq!(
            journal.complete("op", Completion::Uncertain),
            Err(JournalError::AlreadyReserved)
        );
        drop(journal);
        let journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        assert_eq!(
            journal.status("op"),
            Some(Status::Acknowledged(crate::ProtocolReference::Signal {
                timestamp: 42
            }))
        );
    }

    #[test]
    fn corrupt_ciphertext_never_resets_the_send_ledger() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        drop(journal);
        let path = root.path().join(secret_vault::VAULT_FILE);
        let mut corrupt = std::fs::read(&path).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &corrupt).unwrap();
        assert!(EncryptedJournal::open(root.path(), &owner(), key()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    #[test]
    fn an_acknowledgment_from_another_protocol_cannot_finish_this_send() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = EncryptedJournal::open(root.path(), &owner(), key()).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        assert_eq!(
            journal.complete(
                "op",
                Completion::Acknowledged(crate::ProtocolReference::Whatsapp {
                    message_id: "op".into()
                })
            ),
            Err(JournalError::Unavailable)
        );
        assert_eq!(journal.status("op"), Some(Status::Started));
    }

    #[test]
    fn existing_custody_is_lookup_only_and_missing_keys_do_not_reset_journals() {
        let root = tempfile::tempdir().unwrap();
        let store = tilo_secrets::SecretStore::memory("so.tilo.messaging-broker");
        assert!(EncryptedJournal::open_existing_in(root.path(), &owner(), &store).is_err());
        assert!(store.get_blocking("send-journal").unwrap().is_none());
        assert!(!root.path().join(secret_vault::VAULT_FILE).exists());
        // Explicit fixture provisioning uses the real secret-vault API and
        // an isolated memory backend; production lookup never provisions.
        FORMAT.load_or_create_system_key_in(false, &store).unwrap();
        let mut journal =
            EncryptedJournal::open_existing_in(root.path(), &owner(), &store).unwrap();
        journal.begin("op", &route(), b"private").unwrap();
        drop(journal);
        let path = root.path().join(secret_vault::VAULT_FILE);
        let original = std::fs::read(&path).unwrap();
        store.delete_blocking("send-journal").unwrap();
        assert!(EncryptedJournal::open_existing_in(root.path(), &owner(), &store).is_err());
        assert!(store.get_blocking("send-journal").unwrap().is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!root.path().join(secret_vault::KEY_FILE).exists());
    }

    #[test]
    fn system_runtime_refuses_local_or_memory_custody_and_wrong_store_scope() {
        let root = tempfile::tempdir().unwrap();
        let _scope = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        assert!(EncryptedJournal::open_system(root.path(), &owner()).is_err());
        let other = tilo_secrets::SecretStore::memory("another-app");
        assert!(EncryptedJournal::open_existing_in(root.path(), &owner(), &other).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn submitted_sdk_write_is_durable_without_inventing_server_acknowledgment() {
        let root = tempfile::tempdir().unwrap();
        let owner = Owner {
            service: Service::Whatsapp,
            ..owner()
        };
        let route = Route::new(Service::Whatsapp, "account-a".into(), "123@lid".into()).unwrap();
        let reference = crate::ProtocolReference::Whatsapp {
            message_id: "op".into(),
        };
        let mut journal = EncryptedJournal::open(root.path(), &owner, key()).unwrap();
        journal.begin("op", &route, b"exact SDK inputs").unwrap();
        journal
            .complete("op", Completion::Submitted(reference.clone()))
            .unwrap();
        drop(journal);
        let mut reopened = EncryptedJournal::open(root.path(), &owner, key()).unwrap();
        assert_eq!(reopened.status("op"), Some(Status::Submitted(reference)));
        assert_eq!(
            reopened.begin("op", &route, b"exact SDK inputs"),
            Err(JournalError::AlreadyReserved)
        );
    }
}
