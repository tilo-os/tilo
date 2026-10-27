//! Closed evidence from canonical, identity-pinned native enrollment.
use crate::{PendingEnrollmentId, RoomId};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{
    DeliveryOriginLocatorV1, IrohWorkspaceHostHintV1, MAX_WORKSPACE_JOIN_REQUEST_BYTES,
    TRANSPORT_DELIVERY_ORIGIN_V1, WorkspaceHostMode,
};
use tilo_peer_state::join::{
    self, JoinBootstrap, JoinCompleteRequest, JoinTransportRequest,
    TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1, TransportHint, VerifiedJoin,
};
use tilo_user_identity::{DeviceCert, RevocationList};

const MAX_INTENT_BYTES: usize = MAX_WORKSPACE_JOIN_REQUEST_BYTES + 8192;
#[derive(Clone, Serialize, Deserialize)]
struct IntentRecord {
    version: u8,
    prepared_at_ns: u64,
    local_device: DeviceCert,
    complete: Vec<u8>,
    hint_index: u32,
}
/// Historical signed proof custody, not current workspace or media authority.
#[derive(Clone)]
pub struct VerifiedNativeEnrollmentIntent {
    record: IntentRecord,
    join: VerifiedJoin,
    delivery_locator: Option<TransportHint>,
}
impl std::fmt::Debug for VerifiedNativeEnrollmentIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedNativeEnrollmentIntent")
            .field("room", &self.room())
            .finish_non_exhaustive()
    }
}
impl VerifiedNativeEnrollmentIntent {
    #[must_use]
    pub fn verified_join(&self) -> &VerifiedJoin {
        &self.join
    }
    #[must_use]
    pub fn local_device(&self) -> &DeviceCert {
        &self.record.local_device
    }
    #[allow(
        clippy::indexing_slicing,
        reason = "`validate` rejects an out-of-range `hint_index` before an intent exists"
    )]
    #[must_use]
    pub fn transport_hint(&self) -> &TransportHint {
        &self.join.grant().transport[self.record.hint_index as usize]
    }
    #[allow(
        clippy::expect_used,
        reason = "`validate` rejects a noncanonical grant room before an intent exists"
    )]
    /// # Panics
    /// Never: `validate` rejects a noncanonical grant room.
    #[must_use]
    pub fn room(&self) -> RoomId {
        RoomId::parse(&self.join.grant().room).expect("checked canonical room")
    }
    #[must_use]
    pub fn complete_bytes(&self) -> &[u8] {
        &self.record.complete
    }
    #[must_use]
    pub fn proof_digest(&self) -> [u8; 32] {
        *blake3::hash(self.complete_bytes()).as_bytes()
    }
    #[must_use]
    pub fn delivery_host(&self) -> Option<&DeviceCert> {
        self.delivery_locator.as_ref().map(|hint| &hint.host)
    }
    #[must_use]
    pub fn delivery_locator(&self) -> Option<&TransportHint> {
        self.delivery_locator.as_ref()
    }
    /// # Errors
    /// The intent exceeds its bound.
    pub fn durable_bytes(&self) -> Result<Vec<u8>, String> {
        let bytes = postcard::to_stdvec(&self.record).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_INTENT_BYTES {
            return Err("native enrollment intent exceeds bound".into());
        }
        Ok(bytes)
    }
    /// # Errors
    /// `bytes` are not a canonical, valid intent.
    pub fn restore(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_INTENT_BYTES {
            return Err("native enrollment intent exceeds bound".into());
        }
        let record: IntentRecord = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
        if postcard::to_stdvec(&record).map_err(|e| e.to_string())? != bytes {
            return Err("noncanonical native enrollment intent".into());
        }
        Self::validate(record)
    }
    fn validate(record: IntentRecord) -> Result<Self, String> {
        if record.version != 1
            || record.complete.len() > MAX_WORKSPACE_JOIN_REQUEST_BYTES
            || record.prepared_at_ns > now_ns(SystemTime::now())?
        {
            return Err("invalid native enrollment intent version, time or bound".into());
        }
        let request: JoinTransportRequest =
            postcard::from_bytes(&record.complete).map_err(|e| e.to_string())?;
        if postcard::to_stdvec(&request).map_err(|e| e.to_string())? != record.complete {
            return Err("noncanonical native Complete".into());
        }
        let JoinTransportRequest::Complete(complete) = request else {
            return Err("native intent is not Complete".into());
        };
        let redemption = join::verify_join_proof(
            &complete.proof,
            &complete.proof.challenge,
            &record.local_device,
            &RevocationList::new(),
            record.prepared_at_ns,
        )?;
        let join = redemption.join().clone();
        let challenge = &complete.proof.challenge;
        let expected = join::JoinChallenge::new(
            &join,
            &record.local_device,
            challenge.request_id,
            challenge.nonce,
            challenge.expires_at_ns,
            record.prepared_at_ns,
        )?;
        if &expected != challenge {
            return Err("native Complete challenge scope mismatch".into());
        }
        let hint = join
            .grant()
            .transport
            .get(record.hint_index as usize)
            .ok_or("native Iroh hint index missing")?;
        validate_historical_iroh(&join, hint, record.prepared_at_ns)?;
        let mut locators = join
            .grant()
            .transport
            .iter()
            .filter(|hint| hint.kind == TRANSPORT_DELIVERY_ORIGIN_V1);
        let delivery_locator = locators.next().cloned();
        if locators.next().is_some() {
            return Err("ambiguous native delivery host appointment".into());
        }
        if let Some(locator) = &delivery_locator {
            DeliveryOriginLocatorV1::from_transport_hint(locator, &locator.host)
                .map_err(|e| e.to_string())?;
            if !locator.host.capabilities.can_act_as_host {
                return Err("appointed delivery certificate cannot host".into());
            }
            // The original invitation signature appoints this exact full certificate.
            join::verify_transport_hint_at(locator, &RevocationList::new(), record.prepared_at_ns)?;
        }
        Ok(Self {
            record,
            join,
            delivery_locator,
        })
    }
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the exchange hands over the proof inputs it produced; none outlive the intent"
    )]
    /// The intent to complete `join` with `complete`, the proof signed for
    /// the challenge the host issued over `hint`.
    ///
    /// # Errors
    /// `hint` was not appointed by the invitation, or the proof does not
    /// verify for it.
    pub(crate) fn from_proof(
        join: VerifiedJoin,
        complete: JoinCompleteRequest,
        local_device: DeviceCert,
        hint: TransportHint,
        prepared_at: SystemTime,
    ) -> Result<Self, String> {
        let mut positions = join
            .grant()
            .transport
            .iter()
            .enumerate()
            .filter(|(_, candidate)| **candidate == hint);
        let index = positions
            .next()
            .ok_or("used Iroh hint was not appointed by signed invitation")?
            .0;
        if positions.next().is_some() {
            return Err("ambiguous used Iroh hint".into());
        }
        let record = IntentRecord {
            version: 1,
            prepared_at_ns: now_ns(prepared_at)?,
            local_device,
            complete: postcard::to_stdvec(&JoinTransportRequest::Complete(complete))
                .map_err(|e| e.to_string())?,
            hint_index: u32::try_from(index).map_err(|e| e.to_string())?,
        };
        let intent = Self::validate(record)?;
        if intent.join != join {
            return Err("Complete replaced original verified invitation".into());
        }
        intent.durable_bytes()?;
        Ok(intent)
    }
}
// Recovery verifies the historical hint at its preparation time. Live dialing
// additionally uses authenticate_transport_hint, which verifies current certificates.
fn validate_historical_iroh(
    join: &VerifiedJoin,
    hint: &TransportHint,
    at: u64,
) -> Result<(), String> {
    if hint.kind != TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1 {
        return Err("native enrollment requires signed Iroh host hint".into());
    }
    join::verify_transport_hint_at(hint, &RevocationList::new(), at)?;
    let value: IrohWorkspaceHostHintV1 =
        serde_json::from_str(&hint.opaque_json).map_err(|e| e.to_string())?;
    if value.version != 1
        || value.room != join.grant().room
        || value.workspace_id != join.grant().workspace_id
        || !tilo_relay_protocol::room_workspace_id_matches(&value.room, value.workspace_id)
        || RoomId::parse(&value.room).is_none()
        || value.governing_authority != join.grant().authority
        || value.host_device_certificate != hint.host
        || value.host_user_public_key != hint.host.user_public_key
        || *value.endpoint_addr.id.as_bytes() != hint.service_public_key
        || value.packet_relay_url.is_empty()
        || value.packet_relay_url.len() > 4096
    {
        return Err("native signed Iroh scope mismatch".into());
    }
    tilo_user_identity::verify_device_cert_at(&value.governing_authority, at)
        .map_err(|e| e.to_string())?;
    match value.mode {
        WorkspaceHostMode::CertifiedHost if !hint.host.capabilities.can_act_as_host => {
            return Err("native data host lacks host capability".into());
        }
        WorkspaceHostMode::NativeInteractiveWorkspaceOwner
            if hint.host != value.governing_authority
                || !hint.host.capabilities.can_own_workspaces =>
        {
            return Err("interactive data host is not exact governor".into());
        }
        _ => {}
    }
    value
        .validate_invitation(join.grant())
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn now_ns(now: SystemTime) -> Result<u64, String> {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
    )
    .map_err(|e| e.to_string())
}

/// Constructed only after an authenticated Complete response and a persisted intent.
#[derive(Clone, Debug)]
pub struct VerifiedNativeEnrollment {
    intent: VerifiedNativeEnrollmentIntent,
    pending_id: PendingEnrollmentId,
    bootstrap: JoinBootstrap,
}
impl VerifiedNativeEnrollment {
    #[must_use]
    pub fn verified_join(&self) -> &VerifiedJoin {
        self.intent.verified_join()
    }
    #[must_use]
    pub fn bootstrap(&self) -> &JoinBootstrap {
        &self.bootstrap
    }
    /// The certificate this device is a member with: the one it joined
    /// with, or the renewal of it the governor answered.
    #[must_use]
    pub fn local_device(&self) -> &DeviceCert {
        &self.bootstrap.device_cert
    }
    #[must_use]
    pub fn transport_hint(&self) -> &TransportHint {
        self.intent.transport_hint()
    }
    #[must_use]
    pub fn delivery_hint(&self) -> Option<&TransportHint> {
        None
    }
    #[must_use]
    pub fn pending_id(&self) -> &PendingEnrollmentId {
        &self.pending_id
    }
    #[allow(
        clippy::suspicious_operation_groupings,
        reason = "a pending id's `digest` is the intent's proof digest"
    )]
    /// The enrollment `bootstrap` completes. Only the Complete response of
    /// the pinned host that issued `intent`'s challenge may be passed here.
    ///
    /// # Errors
    /// `bootstrap` does not match the persisted proof and its exact scope.
    pub(crate) fn finalize(
        intent: VerifiedNativeEnrollmentIntent,
        pending_id: PendingEnrollmentId,
        bootstrap: JoinBootstrap,
    ) -> Result<Self, String> {
        let grant = intent.verified_join().grant();
        if pending_id.room() != intent.room()
            || pending_id.digest() != intent.proof_digest()
            || pending_id.generation() == 0
            || bootstrap.version != 1
            || bootstrap.invite_id != intent.verified_join().invite_id()
            || (bootstrap.device_cert != *intent.local_device()
                && !crate::renewal::renews(&bootstrap.device_cert, intent.local_device()))
            || bootstrap.role != grant.role
            || bootstrap.room != grant.room
            || bootstrap.workspace_id != grant.workspace_id
            || bootstrap.authority != grant.authority
            || bootstrap.generation == 0
        {
            return Err(
                "native Complete does not match persisted proof and exact enrollment scope".into(),
            );
        }
        if !bootstrap.certified_roster.contains(&bootstrap.device_cert)
            || !bootstrap.certified_roster.contains(&grant.authority)
        {
            return Err("native Complete omitted exact enrolled certificates".into());
        }
        let now = SystemTime::now();
        let mut revocations = RevocationList::new();
        for entry in &bootstrap.revocations {
            revocations
                .insert(entry.clone())
                .map_err(|e| e.to_string())?;
        }
        // Actual StatePeer validation checks certificate/revocation and authority input.
        // A alone admits the signed governance and publishes the durable peer.
        // The roster is the room's admitted evidence: a guest whose
        // certificate expired since is still in it.
        tilo_peer_state::StatePeer::joined(
            &bootstrap.room,
            bootstrap.authority.clone(),
            bootstrap.certified_roster.clone(),
            revocations,
            now,
        )?;
        Ok(Self {
            intent,
            pending_id,
            bootstrap,
        })
    }
}
#[cfg(test)]
#[path = "certified_tests.rs"]
mod tests;
