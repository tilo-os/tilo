//! Reusable carrier machinery, never a concrete owner qualification.
//! Positive capabilities belong only to a fully installed concrete owner.
pub use crate::host_bytes::{ByteCredit, HostBytes};
pub use crate::https::{BufferedRequest, ResponseCredits, Route};
#[cfg(feature = "media")]
pub use crate::media_service::{
    MediaAuthoritySource, MediaAuthorityTurn, MediaObservation, MediaService, MediaSigningInput,
    RoomMediaPolicy,
};
pub use crate::private_http::{
    PendingPrivate, PrivateError, PrivateService as PrivateCarrier, WatchId, WatchUpdate,
};

pub use crate::delivery_http::HttpSessions as CarrierRegistry;
pub use crate::delivery_http::carrier::{CarrierFinish, CarrierIntent, CarrierReady};

#[cfg(feature = "media")]
pub use crate::media_ingress::{PreparedTransport, transport::RunningTransport};
pub use crate::store::{
    HostWorkFailure, HostWorkInventory, LargeDecodeLease, StageIdentity, StagingDirectory,
    StagingReservation,
};

#[cfg(feature = "media")]
pub use crate::media_authority::MediaError;
#[cfg(feature = "media")]
pub use qos_moq::relay::{
    AuthorizationStatus, Denied, Direction, RelayAuthority, SessionAuthorization,
};

pub use crate::delivery_http::revalidation::continuation::{
    AdmissionContinuation as CarrierTicket, PendingOperation as PendingCarrier,
};

pub use crate::delivery_http::upload::{CompletedUpload, UploadChunk, UploadFailure, UploadStage};

pub use crate::publication_custody::{PublicationCustody, PublicationCustodyFailure};

// Resource and signed-byte custody only; concrete owners retain pin authority.
pub use crate::delivery_http::snapshot::pair::{
    AuthenticatedPair as AuthenticatedSnapshotPair, ManifestPair as SnapshotManifestPair,
    RetainedAuthenticatedPair as RetainedAuthenticatedSnapshotPair,
};
pub use crate::delivery_http::snapshot::{SnapshotFailure, SnapshotStream};

pub mod delivery_planning {
    pub use crate::delivery_planning::*;
}
