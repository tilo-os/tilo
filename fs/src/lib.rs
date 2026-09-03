#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod acl;
pub mod authenticated_transport;
pub mod availability;
pub mod caps_file;
pub mod clock;
pub mod conflict;
pub mod conflict_merge;
pub mod content;
pub mod control;
pub mod docstore;
pub mod durable_backup;
pub mod eviction;
pub mod fs;
pub mod full_replica;
pub mod history;
pub mod hydration;
pub mod identity;
pub mod named_share_delivery;
pub mod namespace;
pub mod peer_auth;
pub mod recipient_control;
pub mod recipient_mount;
pub mod record_access;
pub mod record_materializer;
pub mod record_mesh;
pub mod record_protocol;
pub mod record_publisher;
pub mod record_replication;
pub mod record_repository;
pub mod record_subscription;
pub mod record_transaction;
pub mod replica_ack;
mod rich_metadata;
pub mod share_authority;
pub mod share_control;
pub mod share_link;
pub mod share_management;
pub mod share_session;
pub mod smart;
pub mod smart_monitor;
pub mod storage;
pub mod store;
pub mod sync;
pub mod syncstate;
pub mod thumbs;
pub mod transaction_coordinator;
pub mod transport_gate;

#[cfg(test)]
mod tests;

pub use acl::{Access, AclEntry, AclHandle, IdentityKey, WorkspaceAcl};
pub use authenticated_transport::{
    AuthenticatedTransportConfig, AuthenticatedTransportError, authenticate_fs_peer_at,
    authenticate_fs_peer_parts_at,
};
pub use availability::AvailabilityRegistry;
pub use caps_file::{CapabilityFile, CapsError, Refresh};
pub use collab_types::{
    MAX_RECIPIENT_CONTROL_CONNECTIONS, MAX_RECIPIENT_CONTROL_FRAME_BYTES,
    MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION, RECIPIENT_CONTROL_VERSION,
    RecipientControlErrorCode, RecipientControlOperationV1, RecipientControlRequestV1,
    RecipientControlResponseV1, RecipientControlResultV1, RecipientFailureCodeV1,
    RecipientFailureV1, RecipientSubscriptionKeyV1, RecipientSubscriptionStatusV1,
};
pub use docstore::{DocStore, VersionManifest, VersionRef};
pub use fs::QuickFS;
pub use history::{
    FileHistoryStore, HISTORY_XATTR, HistoryError, HistoryProjectionV1, RecordHistorySnapshot,
};
pub use hydration::Hydrator;
pub use identity::{
    CertifiedIdentityError, CertifiedMountIdentity, MountIdentity, PeerCredentials,
    RevocationListLoadError, WorkspaceShareIssuerError, identity_paths,
    load_certified_mount_identity, load_mount_identity, load_revocation_list,
    load_workspace_share_issuer, revocation_list_path,
};
pub use named_share_delivery::{
    MAX_NAMED_SHARE_INBOX_ENTRIES, NamedShareDeliveryClient, NamedShareDeliveryError,
    NamedShareEnvelopeV1, NamedShareInbox, NamedShareInboxError, NamedShareInboxProtocol,
    NamedShareOutbox, NamedShareOutboxEntryV1, QOS_FS_NAMED_SHARE_INBOX_ALPN,
};
pub use namespace::{Namespace, ROOT_INO, TransactionProjectionError};
pub use peer_auth::{
    AuthenticatedFsPeer, PeerAuthChallenge, PeerAuthError, PeerAuthProof, PendingPeerChallenges,
};
pub use recipient_control::{
    RecipientControlServer, RecipientControlServerError, read_recipient_control_response,
    request_recipient_control, write_recipient_control_request,
};
pub use recipient_mount::{RecipientMountError, RecipientMountInfo, RecipientMountSupervisor};
pub use record_access::{
    MAX_RECORD_ACCESS_ENTRIES, MAX_RECORD_ACCESS_TTL_MS, RecordAccessBinding, RecordAccessRegistry,
    RecordAccessRegistryError, RecordAccessSessionId,
};
pub use record_materializer::{
    RecordMaterializer, RecordMaterializerError, RecordReconciliationReport,
};
pub use record_protocol::{
    MAX_RECORD_PROTOCOL_FRAME_BYTES, QOS_FS_RECORD_ALPN, RECORD_PROTOCOL_VERSION,
    RecordMutationSink, RecordMutationSinkError, RecordProtocol, RecordProtocolClient,
    RecordProtocolClientError, RecordProtocolCredentialV1, RecordProtocolError,
    RecordProtocolOperationV1, RecordProtocolRequestV1, RecordProtocolResponseV1,
    RecordProtocolSessionV1, RecordProtocolSnapshotV1,
};
pub use record_publisher::{
    BindingPublishStatus, LocalOwnerAuthorizer, MetadataPublishStatus, PublishReport,
    RecordPublisher, RecordPublisherError,
};
pub use record_replication::{
    RecordAction, RecordAuthorizer, RecordEnvelopeV1, RecordOperationV1, RecordReplica,
    RecordReplicationError, RecordSnapshot,
};
pub use record_repository::{
    MAX_RECORD_FILE_BYTES, MAX_RECORD_OPERATIONS, MAX_RECORD_VERSIONS, MAX_REPOSITORY_RECORDS,
    RECORD_REPOSITORY_VERSION, RecordRepository, RecordRepositoryError, RecordRepositoryKey,
    RecordTransactionCheckpoint, RecordTransactionStatus,
};
pub use record_subscription::{
    MAX_RECORD_SUBSCRIPTION_FILE_BYTES, MAX_RECORD_SUBSCRIPTION_URI_BYTES,
    MAX_RECORD_SUBSCRIPTIONS, RECORD_SUBSCRIPTION_STORE_VERSION, RecordSubscription,
    RecordSubscriptionFailure, RecordSubscriptionFailureCode, RecordSubscriptionKey,
    RecordSubscriptionStore, RecordSubscriptionStoreError,
};
pub use record_transaction::{
    MAX_RECORD_TRANSACTION_ENTRIES, MAX_RECORD_TRANSACTION_ENTRY_BYTES,
    RECORD_TRANSACTION_JOURNAL_VERSION, RecordTransactionJournal, RecordTransactionJournalEntry,
    RecordTransactionJournalError, RecordTransactionParticipantBatch, RecordTransactionPhase,
};
pub use replica_ack::{
    MAX_REPLICA_ACK_FRAME_BYTES, MAX_REPLICA_ACK_LIFETIME_MS, QOS_FS_REPLICA_ACK_ALPN,
    REPLICA_ACK_PROTOCOL_VERSION, ReplicaAckError, ReplicaAckRequestV1, ReplicaAckResponseV1,
    ReplicaAckV1,
};
pub use share_authority::{
    MAX_SHARE_PROJECTION_BYTES, MAX_SHARES_PER_RECORD, Redemption, SHARE_PROJECTION_VERSION,
    ShareAuthority, ShareAuthorityError, ShareGrantProjectionV1, ShareGrantStatus,
    ShareProjectionError, ShareProjectionV1, ShareSubjectProjection,
};
pub use share_control::{
    MAX_SHARE_CONTROL_CONNECTIONS, MAX_SHARE_CONTROL_FRAME_BYTES,
    MAX_SHARE_CONTROL_REQUESTS_PER_CONNECTION, NamedGrantMetadataV1, SHARE_CONTROL_VERSION,
    ShareControlClock, ShareControlErrorCode, ShareControlOperationV1, ShareControlRequestV1,
    ShareControlResponseV1, ShareControlResultV1, ShareControlServer, ShareControlServerError,
    SystemShareControlClock, read_share_control_response, write_share_control_request,
};
pub use share_link::{FileShareLinkError, FileShareLinkV2};
pub use share_management::{
    BearerLinkRequest, MAX_BEARER_REDEMPTIONS, MAX_SHARE_LIFETIME_MS, MAX_TRUSTED_SHARE_ISSUERS,
    NamedGrantRequest, ShareManagement, ShareManagementError,
};
pub use share_session::{
    LiveActorCertification, ShareSession, ShareSessionClock, ShareSessionError,
    SystemShareSessionClock,
};
pub use smart::Heuristics;
pub use store::{BackendGcRelease, ContentStore, DEFAULT_BLOB_GC_INTERVAL, StoreEvictionError};
pub use sync::{FsTransportIdentity, RecordServices, Ticket, Workspace};
pub use syncstate::{SHARE_CONTROL_XATTR, SHARE_XATTR, SYNC_XATTR, SyncState, THUMB_XATTR};
pub use thumbs::ThumbProvider;
pub use transaction_coordinator::{
    RecordTransactionCoordinatorError, complete_replacement_transaction,
};
