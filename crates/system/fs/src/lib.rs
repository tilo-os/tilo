//! Portable content-addressed storage, plus the default `filesystem` service.
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "filesystem")]
pub mod acl;
#[cfg(feature = "filesystem")]
pub mod authenticated_transport;
#[cfg(feature = "filesystem")]
pub mod availability;
#[cfg(feature = "filesystem")]
mod blob_access;
#[cfg(feature = "filesystem")]
mod canonical_frame;
#[cfg(feature = "filesystem")]
pub mod caps_file;
#[cfg(feature = "filesystem")]
pub mod clock;
#[cfg(all(feature = "filesystem", target_os = "linux"))]
pub mod cloud;
#[cfg(feature = "filesystem")]
pub mod conflict;
#[cfg(feature = "filesystem")]
pub mod conflict_merge;
pub mod content;
#[cfg(feature = "filesystem")]
pub mod content_kind;
#[cfg(feature = "filesystem")]
pub mod control;
#[cfg(feature = "filesystem")]
pub mod docstore;
#[cfg(feature = "document-media")]
pub mod document_media;
#[cfg(feature = "filesystem")]
pub mod durable_backup;
#[cfg(feature = "filesystem")]
pub mod eviction;
#[cfg(feature = "filesystem")]
pub mod fs;
#[cfg(feature = "filesystem")]
pub mod full_replica;
#[cfg(feature = "filesystem")]
pub mod history;
#[cfg(feature = "filesystem")]
pub mod hydration;
#[cfg(feature = "filesystem")]
pub mod identity;
#[cfg(feature = "filesystem")]
pub mod named_share_delivery;
#[cfg(feature = "filesystem")]
pub mod namespace;
#[cfg(feature = "filesystem")]
pub mod namespace_binding;
#[cfg(all(feature = "filesystem", target_os = "linux"))]
pub mod native;
#[cfg(feature = "filesystem")]
pub mod peer_auth;
#[cfg(feature = "filesystem")]
pub mod recipient_control;
#[cfg(feature = "filesystem")]
pub mod recipient_mount;
#[cfg(feature = "filesystem")]
pub mod record_access;
#[cfg(feature = "filesystem")]
pub mod record_changeset;
#[cfg(feature = "filesystem")]
mod record_log;
#[cfg(feature = "filesystem")]
pub mod record_materializer;
#[cfg(feature = "filesystem")]
pub mod record_mesh;
#[cfg(feature = "filesystem")]
pub mod record_protocol;
#[cfg(feature = "filesystem")]
pub mod record_publisher;
#[cfg(feature = "filesystem")]
pub mod record_reconcile;
#[cfg(feature = "filesystem")]
pub mod record_replication;
#[cfg(feature = "filesystem")]
pub mod record_repository;
#[cfg(feature = "filesystem")]
pub mod record_subscription;
#[cfg(feature = "filesystem")]
pub mod record_transaction;
#[cfg(feature = "filesystem")]
pub mod replica_ack;
#[cfg(feature = "filesystem")]
mod rich_metadata;
#[cfg(feature = "filesystem")]
pub mod share_authority;
#[cfg(feature = "filesystem")]
pub mod share_control;
#[cfg(feature = "filesystem")]
pub mod share_link;
#[cfg(feature = "filesystem")]
pub mod share_management;
#[cfg(feature = "filesystem")]
pub mod share_session;
#[cfg(feature = "filesystem")]
pub mod smart;
#[cfg(feature = "filesystem")]
pub mod smart_monitor;
#[cfg(feature = "filesystem")]
pub mod storage;
pub mod store;
#[cfg(feature = "filesystem")]
pub mod sync;
#[cfg(feature = "filesystem")]
pub mod sync_folder;
#[cfg(feature = "filesystem")]
pub mod syncstate;
#[cfg(feature = "filesystem")]
pub mod thumbs;
#[cfg(feature = "filesystem")]
pub mod transaction_coordinator;
#[cfg(feature = "filesystem")]
pub mod transport_gate;
#[cfg(feature = "filesystem")]
pub mod unsaved;
#[cfg(feature = "filesystem")]
pub mod workspace_authority;

#[cfg(test)]
#[cfg(feature = "filesystem")]
mod tests;

// The qdoc conflict merge decodes peer versions in Automerge workers.
#[cfg(test)]
#[cfg(feature = "filesystem")]
tilo_automerge_isolate::test_worker!();

#[cfg(feature = "filesystem")]
pub use acl::{Access, AclEntry, AclHandle, IdentityKey, WorkspaceAcl};
#[cfg(feature = "filesystem")]
pub use authenticated_transport::{
    AuthenticatedTransportConfig, AuthenticatedTransportError, authenticate_fs_peer_at,
    authenticate_fs_peer_parts_at,
};
#[cfg(feature = "filesystem")]
pub use availability::AvailabilityRegistry;
#[cfg(feature = "filesystem")]
pub use caps_file::{CapabilityFile, CapsError, Refresh};
#[cfg(feature = "filesystem")]
pub use docstore::{DocStore, VersionManifest, VersionRef};
#[cfg(feature = "filesystem")]
pub use fs::QuickFS;
#[cfg(feature = "filesystem")]
pub use fs_protocol::{
    MAX_RECIPIENT_CONTROL_CONNECTIONS, MAX_RECIPIENT_CONTROL_FRAME_BYTES,
    MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION, RECIPIENT_CONTROL_VERSION,
    RecipientControlErrorCode, RecipientControlOperationV1, RecipientControlRequestV1,
    RecipientControlResponseV1, RecipientControlResultV1, RecipientFailureCodeV1,
    RecipientFailureV1, RecipientSubscriptionKeyV1, RecipientSubscriptionStatusV1,
};
#[cfg(feature = "filesystem")]
pub use history::{
    FileHistoryStore, HISTORY_XATTR, HistoryError, HistoryProjectionV1, RecordHistorySnapshot,
};
#[cfg(feature = "filesystem")]
pub use hydration::Hydrator;
#[cfg(feature = "filesystem")]
pub use identity::{
    CertifiedIdentityError, CertifiedMountIdentity, MountIdentity, PeerCredentials,
    RevocationListLoadError, WorkspaceShareIssuerError, identity_paths,
    load_certified_mount_identity, load_mount_identity, load_revocation_list,
    load_workspace_share_issuer, revocation_list_path,
};
#[cfg(feature = "filesystem")]
pub use named_share_delivery::{
    MAX_NAMED_SHARE_INBOX_ENTRIES, NamedShareDeliveryClient, NamedShareDeliveryError,
    NamedShareEnvelopeV1, NamedShareInbox, NamedShareInboxError, NamedShareInboxProtocol,
    NamedShareOutbox, NamedShareOutboxEntryV1, TILO_FS_NAMED_SHARE_INBOX_ALPN,
};
#[cfg(feature = "filesystem")]
pub use namespace::{Namespace, ROOT_INO, TransactionProjectionError};
#[cfg(feature = "filesystem")]
pub use peer_auth::{
    AuthenticatedFsPeer, PeerAuthChallenge, PeerAuthError, PeerAuthProof, PendingPeerChallenges,
};
#[cfg(feature = "filesystem")]
pub use recipient_control::{
    RecipientControlServer, RecipientControlServerError, read_recipient_control_response,
    request_recipient_control, write_recipient_control_request,
};
#[cfg(feature = "filesystem")]
pub use recipient_mount::{RecipientMountError, RecipientMountInfo, RecipientMountSupervisor};
#[cfg(feature = "filesystem")]
pub use record_access::{
    MAX_RECORD_ACCESS_ENTRIES, MAX_RECORD_ACCESS_TTL_MS, RecordAccessBinding, RecordAccessRegistry,
    RecordAccessRegistryError, RecordAccessSessionId,
};
#[cfg(feature = "filesystem")]
pub use record_materializer::{
    RecordMaterializer, RecordMaterializerError, RecordReconciliationReport,
};
#[cfg(feature = "filesystem")]
pub use record_protocol::{
    MAX_RECORD_PROTOCOL_FRAME_BYTES, RECORD_PROTOCOL_VERSION, RecordMutationSink,
    RecordMutationSinkError, RecordProtocol, RecordProtocolClient, RecordProtocolClientError,
    RecordProtocolCredentialV1, RecordProtocolError, RecordProtocolOperationV1,
    RecordProtocolRequestV1, RecordProtocolResponseV1, RecordProtocolSessionV1,
    RecordProtocolSnapshotV1, TILO_FS_RECORD_ALPN,
};
#[cfg(feature = "filesystem")]
pub use record_publisher::{
    BindingPublishStatus, LocalOwnerAuthorizer, MetadataPublishStatus, PublishReport,
    RecordPublisher, RecordPublisherError,
};
#[cfg(feature = "filesystem")]
pub use record_replication::{
    RecordAction, RecordAuthorizer, RecordEnvelopeV1, RecordOperationV1, RecordReplica,
    RecordReplicationError, RecordSnapshot,
};
#[cfg(feature = "filesystem")]
pub use record_repository::{
    MAX_RECORD_OPERATIONS, MAX_RECORD_VERSIONS, MAX_REPOSITORY_RECORDS, RecordRepository,
    RecordRepositoryError, RecordRepositoryKey, RecordTransactionCheckpoint,
    RecordTransactionStatus,
};
#[cfg(feature = "filesystem")]
pub use record_subscription::{
    MAX_RECORD_SUBSCRIPTION_FILE_BYTES, MAX_RECORD_SUBSCRIPTION_URI_BYTES,
    MAX_RECORD_SUBSCRIPTIONS, RECORD_SUBSCRIPTION_STORE_VERSION, RecordSubscription,
    RecordSubscriptionFailure, RecordSubscriptionFailureCode, RecordSubscriptionKey,
    RecordSubscriptionStore, RecordSubscriptionStoreError,
};
#[cfg(feature = "filesystem")]
pub use record_transaction::{
    MAX_RECORD_TRANSACTION_ENTRIES, MAX_RECORD_TRANSACTION_ENTRY_BYTES,
    RECORD_TRANSACTION_JOURNAL_VERSION, RecordTransactionJournal, RecordTransactionJournalEntry,
    RecordTransactionJournalError, RecordTransactionParticipantBatch, RecordTransactionPhase,
};
#[cfg(feature = "filesystem")]
pub use replica_ack::{
    MAX_REPLICA_ACK_FRAME_BYTES, MAX_REPLICA_ACK_LIFETIME_MS, REPLICA_ACK_PROTOCOL_VERSION,
    ReplicaAckError, ReplicaAckRequestV1, ReplicaAckResponseV1, ReplicaAckV1,
    TILO_FS_REPLICA_ACK_ALPN,
};
#[cfg(feature = "filesystem")]
pub use share_authority::{
    MAX_SHARE_PROJECTION_BYTES, MAX_SHARES_PER_RECORD, Redemption, SHARE_PROJECTION_VERSION,
    ShareAuthority, ShareAuthorityError, ShareGrantProjectionV1, ShareGrantStatus,
    ShareProjectionError, ShareProjectionV1, ShareSubjectProjection,
};
#[cfg(feature = "filesystem")]
pub use share_control::{
    MAX_SHARE_CONTROL_CONNECTIONS, MAX_SHARE_CONTROL_FRAME_BYTES,
    MAX_SHARE_CONTROL_REQUESTS_PER_CONNECTION, NamedGrantMetadataV1, SHARE_CONTROL_VERSION,
    ShareControlErrorCode, ShareControlOperationV1, ShareControlRequestV1, ShareControlResponseV1,
    ShareControlResultV1, ShareControlServer, ShareControlServerError, read_share_control_response,
    write_share_control_request,
};
#[cfg(feature = "filesystem")]
pub use share_link::{FileShareLinkError, FileShareLinkV2};
#[cfg(feature = "filesystem")]
pub use share_management::{
    BearerLinkRequest, MAX_BEARER_REDEMPTIONS, MAX_SHARE_LIFETIME_MS, MAX_TRUSTED_SHARE_ISSUERS,
    NamedGrantRequest, ShareManagement, ShareManagementError,
};
#[cfg(feature = "filesystem")]
pub use share_session::{LiveActorCertification, ShareSession, ShareSessionError};
#[cfg(feature = "filesystem")]
pub use smart::Heuristics;
#[cfg(feature = "filesystem")]
pub use store::{BackendGcRelease, StoreEvictionError};
pub use store::{ContentStore, DEFAULT_BLOB_GC_INTERVAL};
#[cfg(feature = "filesystem")]
pub use sync::{FsTransportIdentity, RecordServices, Ticket, Workspace};
#[cfg(feature = "filesystem")]
pub use syncstate::{SHARE_CONTROL_XATTR, SHARE_XATTR, SYNC_XATTR, SyncState, THUMB_XATTR};
#[cfg(feature = "filesystem")]
pub use thumbs::ThumbProvider;
#[cfg(feature = "filesystem")]
pub use transaction_coordinator::{
    RecordTransactionCoordinatorError, complete_replacement_transaction,
};

#[cfg(test)]
#[cfg(all(target_os = "macos", feature = "filesystem"))]
mod native_mount_selection_tests {
    #[test]
    fn unselected_native_mount_fails_without_mounting() {
        struct EmptyFilesystem;
        impl fuser::Filesystem for EmptyFilesystem {}
        let mountpoint = tempfile::tempdir().unwrap();
        let result = fuser::Session::new(
            EmptyFilesystem,
            mountpoint.path(),
            &fuser::Config::default(),
        );
        let error = match result {
            Ok(_) => panic!("native mounting must remain unavailable"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            error.to_string(),
            "Mount is not enabled; this is test-only configuration"
        );
        assert_eq!(std::fs::read_dir(mountpoint.path()).unwrap().count(), 0);
    }
}

/// Deterministic incompressible bytes for content tests.
#[cfg(test)]
mod test_bytes {
    pub(crate) fn pseudo(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect()
    }
}
