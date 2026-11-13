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
pub mod cache_policy;
#[cfg(feature = "filesystem")]
pub mod caps_file;
#[cfg(feature = "filesystem")]
pub mod change_signal;
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
#[cfg(feature = "document-media")]
pub mod document_media;
#[cfg(feature = "filesystem")]
pub mod durable_backup;
#[cfg(feature = "filesystem")]
pub mod eviction;
#[cfg(feature = "filesystem")]
pub mod file_watch;
#[cfg(feature = "filesystem")]
pub mod fs;
#[cfg(feature = "filesystem")]
pub mod full_replica;
#[cfg(feature = "filesystem")]
pub mod fuse_session;
#[cfg(feature = "filesystem")]
pub mod hydration;
#[cfg(feature = "filesystem")]
pub mod identity;
#[cfg(feature = "filesystem")]
#[cfg(feature = "filesystem")]
pub mod namespace;
#[cfg(feature = "filesystem")]
#[cfg(feature = "filesystem")]
pub mod namespace_persist;
#[cfg(all(feature = "filesystem", target_os = "linux"))]
pub mod native;
#[cfg(feature = "filesystem")]
pub mod peer_auth;
#[cfg(feature = "document-media")]
pub mod public_content;
#[cfg(feature = "filesystem")]
pub mod reachability;
#[cfg(feature = "filesystem")]
pub mod replica_content;
#[cfg(feature = "filesystem")]
mod rich_metadata;
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
pub mod thumbs;
#[cfg(feature = "filesystem")]
pub mod transport_gate;
/// The workspace tree as the Files app's person record.
#[cfg(feature = "filesystem")]
pub mod tree_record;
#[cfg(feature = "filesystem")]
pub mod unsaved;
#[cfg(feature = "filesystem")]
pub mod workspace_authority;
#[cfg(any(feature = "filesystem", feature = "document-media"))]
pub mod write_attribution;

#[cfg(test)]
#[cfg(feature = "filesystem")]
mod history_fixture;
#[cfg(test)]
#[cfg(feature = "filesystem")]
mod tests;

// The tilo-doc conflict merge decodes peer versions in Automerge workers.
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
pub use fs::QuickFS;
#[cfg(feature = "filesystem")]
pub use hydration::Hydrator;
#[cfg(feature = "filesystem")]
pub use identity::{
    CertifiedIdentityError, CertifiedMountIdentity, MountIdentity, PeerCredentials,
    WorkspaceShareIssuerError, identity_paths, load_certified_mount_identity,
    load_certified_mount_identity_once_valid, load_mount_identity, load_revocation_list,
    load_workspace_share_issuer, revocation_list_path,
};
#[cfg(feature = "filesystem")]
pub use namespace::history::{HistoryError, RecordHistorySnapshot};
#[cfg(feature = "filesystem")]
pub use namespace::{Namespace, ROOT_INO, TransactionProjectionError};
#[cfg(feature = "filesystem")]
pub use peer_auth::{
    AuthenticatedFsPeer, PeerAuthChallenge, PeerAuthError, PeerAuthProof, PendingPeerChallenges,
};
#[cfg(feature = "filesystem")]
pub use smart::Heuristics;
#[cfg(feature = "filesystem")]
pub use store::{BackendGcRelease, StoreEvictionError};
pub use store::{ContentStore, DEFAULT_BLOB_GC_INTERVAL};
#[cfg(feature = "filesystem")]
pub use sync::{FsTransportIdentity, Ticket, Workspace};
#[cfg(feature = "filesystem")]
pub use thumbs::ThumbProvider;
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
        let Err(error) = result else {
            panic!("native mounting must remain unavailable")
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            error.to_string(),
            "Mount is not enabled; this is test-only configuration"
        );
        assert_eq!(std::fs::read_dir(mountpoint.path()).unwrap().count(), 0);
    }
}

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
