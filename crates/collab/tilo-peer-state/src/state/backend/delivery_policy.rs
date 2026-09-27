//! Actual-core adapters to the one delivery policy projection.
use super::{BoundedNative, DeviceCert, PeerCore, StatePeer, SystemTime};
use crate::state::bounded::{BoundedPeerError, PeerLimits};
#[derive(Debug, PartialEq, Eq)]
pub enum PeerDeliveryPolicyError {
    Peer(BoundedPeerError),
    Unavailable,
    CurrentDenied,
    UnsupportedPolicy,
    Capacity,
}
impl std::fmt::Display for PeerDeliveryPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "delivery policy refusal: {self:?}")
    }
}
impl std::error::Error for PeerDeliveryPolicyError {}
/// Inline/borrowed policy data, never owner, session, or caller authority.
/// No hash constructor, raw peer, owned buffers or native document is exposed.
/// ```
/// use tilo_peer_state::{PeerRead,PeerDeliveryPolicy,PeerDeliveryPolicyError};
/// fn data<'p>(read:&PeerRead<'p>,cert:&tilo_user_identity::DeviceCert,now:web_time::SystemTime)->Result<PeerDeliveryPolicy<'p>,PeerDeliveryPolicyError>{read.delivery_policy(1,cert,cert,now)}
/// fn fields(value:&PeerDeliveryPolicy<'_>) {let _=(value.room(),value.authority_root(),value.digest());}
/// ```
/// ```compile_fail
/// use tilo_peer_state::PeerDeliveryPolicy;
/// fn escape<'a>(value:PeerDeliveryPolicy<'a>)->PeerDeliveryPolicy<'static>{value}
/// ```
/// ```compile_fail
/// use tilo_peer_state::PeerDeliveryPolicy;
/// let fabricated=PeerDeliveryPolicy {room: "room",authority_root:[0;32],digest:[0;32]};
/// ```
pub struct PeerDeliveryPolicy<'peer> {
    room: &'peer str,
    authority_root: [u8; 32],
    digest: [u8; 32],
}
impl PeerDeliveryPolicy<'_> {
    pub fn room(&self) -> &str {
        self.room
    }
    pub fn authority_root(&self) -> [u8; 32] {
        self.authority_root
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

fn map_error(error: crate::room::delivery_policy::Error) -> PeerDeliveryPolicyError {
    use crate::room::delivery_policy::Error;
    match error {
        Error::Capacity => PeerDeliveryPolicyError::Capacity,
        Error::Decode(error) => PeerDeliveryPolicyError::Peer(BoundedPeerError::Decode(error)),
        Error::Unavailable => PeerDeliveryPolicyError::Unavailable,
        Error::CurrentDenied => PeerDeliveryPolicyError::CurrentDenied,
        Error::UnsupportedPolicy => PeerDeliveryPolicyError::UnsupportedPolicy,
    }
}
pub(in crate::state) fn bounded<'peer>(
    core: &'peer PeerCore<BoundedNative>,
    limits: PeerLimits,
    workspace: u64,
    host: &DeviceCert,
    device: &DeviceCert,
    check_routes: impl FnOnce() -> Result<(), crate::room::delivery_policy::Error>,
) -> Result<PeerDeliveryPolicy<'peer>, PeerDeliveryPolicyError> {
    use crate::room::delivery_policy::{self, Limits, Metadata};
    let room = core
        .room_doc
        .as_ref()
        .ok_or(PeerDeliveryPolicyError::Unavailable)?;
    let read = room.read();
    let digest = delivery_policy::native(
        &read,
        Metadata {
            room: &core.room,
            workspace,
            authority: &core.authority,
            host,
            device,
            certificates: core
                .certificates
                .map()
                .values()
                .map(std::convert::AsRef::as_ref),
            revocations: core.revocations.iter(),
        },
        Limits {
            collections: limits.governance_collection_bytes,
            json: crate::resources::json::JsonLimits {
                allocation_bytes: limits.governance_json.allocation_bytes,
                collection_entries: limits.governance_json.collection_entries,
            },
        },
        check_routes,
    )
    .map_err(map_error)?;
    Ok(PeerDeliveryPolicy {
        room: &core.room,
        authority_root: core.authority.user_public_key,
        digest,
    })
}
impl StatePeer {
    /// Allocating legacy data projection, with no resource/owner qualification.
    /// Host then device are supplied by the parent's authenticated source.
    ///
    /// # Errors
    ///
    /// Returns a [`PeerDeliveryPolicyError`] if a party is not current or a route read check
    /// fails.
    pub fn delivery_policy_unqualified(
        &self,
        workspace: u64,
        host: &DeviceCert,
        device: &DeviceCert,
        now: SystemTime,
    ) -> Result<PeerDeliveryPolicy<'_>, PeerDeliveryPolicyError> {
        use crate::room::{
            delivery_policy::{self, Error, Metadata},
            read::LegacyRead,
        };
        let room = self
            .core
            .room_doc
            .as_deref()
            .ok_or(PeerDeliveryPolicyError::Unavailable)?;
        let digest = delivery_policy::legacy(
            &LegacyRead(room),
            Metadata {
                room: &self.core.room,
                workspace,
                authority: &self.core.authority,
                host,
                device,
                certificates: self
                    .core
                    .certificates
                    .values()
                    .map(std::convert::AsRef::as_ref),
                revocations: self.core.revocations.iter(),
            },
            || {
                for route in self.core.documents.keys() {
                    self.check_document_read(
                        device.device_public_key,
                        &route.app,
                        &route.record,
                        now,
                    )
                    .map_err(|_| Error::CurrentDenied)?;
                }
                Ok(())
            },
        )
        .map_err(map_error)?;
        Ok(PeerDeliveryPolicy {
            room: &self.core.room,
            authority_root: self.core.authority.user_public_key,
            digest,
        })
    }
}
