//! Crash-safe recipient subscriptions to endpoint-bound shared records.
//!
//! The bearer URI (including its fragment secret) exists only in the owner-only
//! state file and private in-memory state. Public snapshots and every error are
//! deliberately URI-free.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use collab_types::{FilePermission, FileRecordId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::clock::unix_now_ms;
use crate::share_link::{
    FILE_SHARE_LINK_V2_PREFIX, FileShareLinkV2, MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES,
};

pub const RECORD_SUBSCRIPTION_STORE_VERSION: u16 = 1;
pub const MAX_RECORD_SUBSCRIPTIONS: usize = 4_096;
pub const MAX_RECORD_SUBSCRIPTION_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_RECORD_SUBSCRIPTION_URI_BYTES: usize =
    FILE_SHARE_LINK_V2_PREFIX.len() + MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES * 2 + 1 + 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordSubscriptionKey {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordSubscriptionFailureCode {
    Offline,
    Authentication,
    Denied,
    NotFound,
    InvalidResponse,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordSubscriptionFailure {
    pub code: RecordSubscriptionFailureCode,
    pub observed_at_ms: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecordSubscription {
    pub key: RecordSubscriptionKey,
    pub endpoint: iroh_base::EndpointAddr,
    pub permission: FilePermission,
    pub last_success_at_ms: Option<u64>,
    pub last_error: Option<RecordSubscriptionFailure>,
    link: FileShareLinkV2,
}

impl RecordSubscription {
    pub fn link(&self) -> &FileShareLinkV2 {
        &self.link
    }
}

impl std::fmt::Debug for RecordSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordSubscription")
            .field("key", &self.key)
            .field("endpoint", &self.endpoint)
            .field("permission", &self.permission)
            .field("last_success_at_ms", &self.last_success_at_ms)
            .field("last_error", &self.last_error)
            .field("link", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
struct StoredSubscription {
    subscription: RecordSubscription,
    uri: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionDocumentV1 {
    schema_version: u16,
    subscriptions: Vec<SubscriptionWireV1>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionWireV1 {
    workspace_id: Uuid,
    record_id: FileRecordId,
    uri: String,
    last_success_at_ms: Option<u64>,
    last_error: Option<RecordSubscriptionFailure>,
}

pub struct RecordSubscriptionStore {
    path: PathBuf,
    state: Mutex<BTreeMap<RecordSubscriptionKey, StoredSubscription>>,
}

impl std::fmt::Debug for RecordSubscriptionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordSubscriptionStore")
            .field("path", &self.path)
            .field("len", &self.state.lock().map(|state| state.len()).ok())
            .finish_non_exhaustive()
    }
}

impl RecordSubscriptionStore {
    /// Open a subscription store using the process security clock.
    ///
    /// # Errors
    ///
    /// Returns an error when state cannot be read, validated, decoded, or
    /// initialized with owner-only permissions.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, RecordSubscriptionStoreError> {
        Self::open_at(path, unix_now_ms())
    }

    /// Open and validate a subscription store at an explicit validation time.
    ///
    /// # Errors
    ///
    /// Returns an error for insecure permissions, oversized/malformed state,
    /// invalid links, duplicate keys, or persistence I/O failure.
    pub fn open_at(
        path: impl Into<PathBuf>,
        now_ms: u64,
    ) -> Result<Self, RecordSubscriptionStoreError> {
        let path = path.into();
        let state = match std::fs::metadata(&path) {
            Ok(metadata) => {
                validate_owner_only_permissions(&metadata)?;
                if metadata.len() > MAX_RECORD_SUBSCRIPTION_FILE_BYTES {
                    return Err(RecordSubscriptionStoreError::StateTooLarge);
                }
                let bytes = std::fs::read(&path)?;
                decode_document(&bytes, now_ms)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        let store = Self {
            path,
            state: Mutex::new(state),
        };
        if !store.path.exists() {
            store.persist(&BTreeMap::new())?;
        }
        Ok(store)
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, key: RecordSubscriptionKey) -> Option<RecordSubscription> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(|stored| stored.subscription.clone())
    }

    pub fn subscriptions(&self) -> Vec<RecordSubscription> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|stored| stored.subscription.clone())
            .collect()
    }

    /// Insert a validated V2 link. Returns `false` for an exact replay and
    /// rejects a different link attempting to occupy the same record key.
    ///
    /// # Errors
    ///
    /// Returns an error when the link is invalid, collides with an existing
    /// key, capacity is exhausted, or persistence fails.
    pub fn insert(&self, link: FileShareLinkV2) -> Result<bool, RecordSubscriptionStoreError> {
        self.insert_at(link, unix_now_ms())
    }

    /// Insert a validated V2 link at an explicit validation time.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::insert`].
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans collision checks, durable replacement, and in-memory publication"
    )]
    pub fn insert_at(
        &self,
        link: FileShareLinkV2,
        now_ms: u64,
    ) -> Result<bool, RecordSubscriptionStoreError> {
        link.validate_at(now_ms)
            .map_err(|_| RecordSubscriptionStoreError::InvalidLink)?;
        let uri = link
            .encode_uri_at(now_ms)
            .map_err(|_| RecordSubscriptionStoreError::InvalidLink)?;
        if uri.len() > MAX_RECORD_SUBSCRIPTION_URI_BYTES {
            return Err(RecordSubscriptionStoreError::InvalidLink);
        }
        let key = RecordSubscriptionKey {
            workspace_id: link.invite.workspace_id,
            record_id: link.invite.record_id,
        };
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = current.get(&key) {
            return if existing.uri == uri {
                Ok(false)
            } else {
                Err(RecordSubscriptionStoreError::DuplicateCollision(key))
            };
        }
        if current.len() >= MAX_RECORD_SUBSCRIPTIONS {
            return Err(RecordSubscriptionStoreError::CapacityReached);
        }
        let mut next = current.clone();
        next.insert(
            key,
            StoredSubscription {
                subscription: RecordSubscription {
                    key,
                    endpoint: link.endpoint.clone(),
                    permission: link.invite.permission,
                    last_success_at_ms: None,
                    last_error: None,
                    link,
                },
                uri,
            },
        );
        self.persist(&next)?;
        *current = next;
        Ok(true)
    }

    /// Record a successful subscription refresh.
    ///
    /// # Errors
    ///
    /// Returns an error when the subscription is unknown or persistence fails.
    pub fn record_success(
        &self,
        key: RecordSubscriptionKey,
        observed_at_ms: u64,
    ) -> Result<(), RecordSubscriptionStoreError> {
        self.update_status(key, |subscription| {
            subscription.last_success_at_ms = Some(observed_at_ms);
            subscription.last_error = None;
        })
    }

    /// Record a coarse, secret-free subscription failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the subscription is unknown or persistence fails.
    pub fn record_error(
        &self,
        key: RecordSubscriptionKey,
        code: RecordSubscriptionFailureCode,
        observed_at_ms: u64,
    ) -> Result<(), RecordSubscriptionStoreError> {
        self.update_status(key, |subscription| {
            subscription.last_error = Some(RecordSubscriptionFailure {
                code,
                observed_at_ms,
            });
        })
    }

    fn update_status(
        &self,
        key: RecordSubscriptionKey,
        update: impl FnOnce(&mut RecordSubscription),
    ) -> Result<(), RecordSubscriptionStoreError> {
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !current.contains_key(&key) {
            return Err(RecordSubscriptionStoreError::UnknownSubscription(key));
        }
        let mut next = current.clone();
        let subscription = next
            .get_mut(&key)
            .ok_or(RecordSubscriptionStoreError::UnknownSubscription(key))?;
        update(&mut subscription.subscription);
        self.persist(&next)?;
        *current = next;
        drop(current);
        Ok(())
    }

    fn persist(
        &self,
        state: &BTreeMap<RecordSubscriptionKey, StoredSubscription>,
    ) -> Result<(), RecordSubscriptionStoreError> {
        let document = SubscriptionDocumentV1 {
            schema_version: RECORD_SUBSCRIPTION_STORE_VERSION,
            subscriptions: state
                .values()
                .map(|stored| SubscriptionWireV1 {
                    workspace_id: stored.subscription.key.workspace_id,
                    record_id: stored.subscription.key.record_id,
                    uri: stored.uri.clone(),
                    last_success_at_ms: stored.subscription.last_success_at_ms,
                    last_error: stored.subscription.last_error,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| RecordSubscriptionStoreError::MalformedState)?;
        if bytes.len() as u64 > MAX_RECORD_SUBSCRIPTION_FILE_BYTES {
            return Err(RecordSubscriptionStoreError::StateTooLarge);
        }
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        tilo_files::native::atomic::write_with_mode(&self.path, &bytes, 0o600)?;
        Ok(())
    }
}

fn decode_document(
    bytes: &[u8],
    now_ms: u64,
) -> Result<BTreeMap<RecordSubscriptionKey, StoredSubscription>, RecordSubscriptionStoreError> {
    let document: SubscriptionDocumentV1 =
        serde_json::from_slice(bytes).map_err(|_| RecordSubscriptionStoreError::MalformedState)?;
    if document.schema_version != RECORD_SUBSCRIPTION_STORE_VERSION {
        return Err(RecordSubscriptionStoreError::UnsupportedVersion);
    }
    if document.subscriptions.len() > MAX_RECORD_SUBSCRIPTIONS {
        return Err(RecordSubscriptionStoreError::CapacityReached);
    }
    let mut state = BTreeMap::new();
    for wire in document.subscriptions {
        if wire.uri.len() > MAX_RECORD_SUBSCRIPTION_URI_BYTES {
            return Err(RecordSubscriptionStoreError::InvalidLink);
        }
        let link = FileShareLinkV2::decode_persisted_uri(&wire.uri)
            .map_err(|_| RecordSubscriptionStoreError::InvalidLink)?;
        let validation_time = link
            .invite
            .expires_at_ms
            .map_or(now_ms, |deadline| deadline.saturating_sub(1));
        let canonical = link
            .encode_uri_at(validation_time)
            .map_err(|_| RecordSubscriptionStoreError::InvalidLink)?;
        let key = RecordSubscriptionKey {
            workspace_id: wire.workspace_id,
            record_id: wire.record_id,
        };
        if key.workspace_id != link.invite.workspace_id || key.record_id != link.invite.record_id {
            return Err(RecordSubscriptionStoreError::KeyMismatch);
        }
        if canonical != wire.uri {
            return Err(RecordSubscriptionStoreError::NonCanonicalState);
        }
        let stored = StoredSubscription {
            subscription: RecordSubscription {
                key,
                endpoint: link.endpoint.clone(),
                permission: link.invite.permission,
                last_success_at_ms: wire.last_success_at_ms,
                last_error: wire.last_error,
                link,
            },
            uri: wire.uri,
        };
        if state.insert(key, stored).is_some() {
            return Err(RecordSubscriptionStoreError::DuplicateCollision(key));
        }
    }
    Ok(state)
}

#[cfg(unix)]
fn validate_owner_only_permissions(
    metadata: &std::fs::Metadata,
) -> Result<(), RecordSubscriptionStoreError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(RecordSubscriptionStoreError::InsecurePermissions);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owner_only_permissions(
    _metadata: &std::fs::Metadata,
) -> Result<(), RecordSubscriptionStoreError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum RecordSubscriptionStoreError {
    #[error("record subscription state I/O failed")]
    Io(#[from] std::io::Error),
    #[error("record subscription state is malformed")]
    MalformedState,
    #[error("record subscription state has an unsupported version")]
    UnsupportedVersion,
    #[error("record subscription state exceeds its bound")]
    StateTooLarge,
    #[error("record subscription capacity is exhausted")]
    CapacityReached,
    #[error("record subscription file is not owner-only mode 0600")]
    InsecurePermissions,
    #[error("record subscription link is invalid")]
    InvalidLink,
    #[error("record subscription state contains a non-canonical link")]
    NonCanonicalState,
    #[error("record subscription key does not match its signed link")]
    KeyMismatch,
    #[error("record subscription key {0:?} collides with another link")]
    DuplicateCollision(RecordSubscriptionKey),
    #[error("record subscription {0:?} does not exist")]
    UnknownSubscription(RecordSubscriptionKey),
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{FileInviteSpec, FileInviteSubject, mint_bearer_file_invite};
    use rand::{SeedableRng, rngs::StdRng};

    const NOW: u64 = 50_000;

    fn link(seed: u64, workspace_id: Uuid, record_id: FileRecordId) -> FileShareLinkV2 {
        link_with_expiry(seed, workspace_id, record_id, None)
    }

    fn link_with_expiry(
        seed: u64,
        workspace_id: Uuid,
        record_id: FileRecordId,
        expires_at_ms: Option<u64>,
    ) -> FileShareLinkV2 {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[u8::try_from(seed).unwrap(); 32]);
        let mut rng = StdRng::seed_from_u64(seed);
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id,
                record_id,
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: FilePermission::View,
                expires_at_ms,
                max_redemptions: None,
            },
            &issuer,
            &mut rng,
        )
        .unwrap();
        let endpoint = iroh_base::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[u8::try_from(seed.wrapping_add(20)).unwrap(); 32])
                .public(),
        )
        .with_ip_addr("127.0.0.1:4455".parse().unwrap());
        FileShareLinkV2::bind_at(endpoint, bearer, &issuer, NOW).unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn subscriptions_persist_privately_fail_closed_and_never_disclose_secrets() {
        // Phase `restart_preserves_link_and_status_in_owner_only_file`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("subscriptions.json");
            let workspace = Uuid::new_v4();
            let record = FileRecordId::new();
            let link = link(1, workspace, record);
            let secret = hex::encode(link.bearer_secret());
            let key = RecordSubscriptionKey {
                workspace_id: workspace,
                record_id: record,
            };
            let store = RecordSubscriptionStore::open_at(&path, NOW).unwrap();
            assert!(store.insert_at(link.clone(), NOW).unwrap());
            assert!(!store.insert_at(link.clone(), NOW).unwrap());
            store
                .record_error(key, RecordSubscriptionFailureCode::Offline, 51_000)
                .unwrap();
            store.record_success(key, 52_000).unwrap();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            let disk = std::fs::read_to_string(&path).unwrap();
            assert!(disk.contains(&secret));
            drop(store);

            let reopened = RecordSubscriptionStore::open_at(&path, NOW).unwrap();
            let subscription = reopened.get(key).unwrap();
            assert_eq!(subscription.link(), &link);
            assert_eq!(subscription.last_success_at_ms, Some(52_000));
            assert_eq!(subscription.last_error, None);
        }
        // Phase `malformed_oversized_duplicate_and_collision_state_fail_closed`.
        {
            let temp = tempfile::tempdir().unwrap();
            let malformed = temp.path().join("malformed.json");
            tilo_files::native::atomic::write_with_mode(&malformed, b"{not-json", 0o600).unwrap();
            assert!(matches!(
                RecordSubscriptionStore::open_at(&malformed, NOW),
                Err(RecordSubscriptionStoreError::MalformedState)
            ));

            let oversized = temp.path().join("oversized.json");
            let file = std::fs::File::create(&oversized).unwrap();
            file.set_len(MAX_RECORD_SUBSCRIPTION_FILE_BYTES + 1)
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&oversized, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            assert!(matches!(
                RecordSubscriptionStore::open_at(&oversized, NOW),
                Err(RecordSubscriptionStoreError::StateTooLarge)
            ));

            let path = temp.path().join("duplicates.json");
            let workspace = Uuid::new_v4();
            let record = FileRecordId::new();
            let first = link(2, workspace, record);
            let store = RecordSubscriptionStore::open_at(&path, NOW).unwrap();
            store.insert_at(first, NOW).unwrap();
            assert!(matches!(
                store.insert_at(link(3, workspace, record), NOW),
                Err(RecordSubscriptionStoreError::DuplicateCollision(_))
            ));
            let mut document: SubscriptionDocumentV1 =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            document
                .subscriptions
                .push(document.subscriptions[0].clone());
            tilo_files::native::atomic::write_with_mode(
                &path,
                &serde_json::to_vec(&document).unwrap(),
                0o600,
            )
            .unwrap();
            assert!(matches!(
                RecordSubscriptionStore::open_at(&path, NOW),
                Err(RecordSubscriptionStoreError::DuplicateCollision(_))
            ));
        }
        // Phase `debug_and_errors_never_disclose_uri_or_secret`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("subscriptions.json");
            let workspace = Uuid::new_v4();
            let record = FileRecordId::new();
            let subscribed_link = link(5, workspace, record);
            let uri = subscribed_link.encode_uri_at(NOW).unwrap();
            let secret = hex::encode(subscribed_link.bearer_secret());
            let store = RecordSubscriptionStore::open_at(&path, NOW).unwrap();
            store.insert_at(subscribed_link, NOW).unwrap();
            let key = RecordSubscriptionKey {
                workspace_id: workspace,
                record_id: record,
            };
            let debug = format!("{:?} {:?}", store, store.get(key).unwrap());
            assert!(!debug.contains(&uri));
            assert!(!debug.contains(&secret));

            let collision = store
                .insert_at(link(6, workspace, record), NOW)
                .unwrap_err();
            let error = format!("{collision:?} {collision}");
            assert!(!error.contains(&uri));
            assert!(!error.contains(&secret));
        }
        // Phase `expired_links_remain_visible_after_restart_but_cannot_be_inserted`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("subscriptions.json");
            let expiring = link_with_expiry(7, Uuid::new_v4(), FileRecordId::new(), Some(NOW + 1));
            let store = RecordSubscriptionStore::open_at(&path, NOW).unwrap();
            store.insert_at(expiring, NOW).unwrap();
            drop(store);

            let reopened = RecordSubscriptionStore::open_at(&path, NOW + 2).unwrap();
            assert_eq!(reopened.len(), 1);
            assert!(
                reopened.subscriptions()[0]
                    .link()
                    .validate_at(NOW + 2)
                    .is_err()
            );

            let other_path = temp.path().join("other.json");
            let expired = link_with_expiry(8, Uuid::new_v4(), FileRecordId::new(), Some(NOW + 1));
            let other = RecordSubscriptionStore::open_at(&other_path, NOW).unwrap();
            assert!(matches!(
                other.insert_at(expired, NOW + 2),
                Err(RecordSubscriptionStoreError::InvalidLink)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn insecure_permissions_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("subscriptions.json");
        let store = RecordSubscriptionStore::open_at(&path, NOW).unwrap();
        store
            .insert_at(link(4, Uuid::new_v4(), FileRecordId::new()), NOW)
            .unwrap();
        drop(store);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            RecordSubscriptionStore::open_at(&path, NOW),
            Err(RecordSubscriptionStoreError::InsecurePermissions)
        ));
    }
}
