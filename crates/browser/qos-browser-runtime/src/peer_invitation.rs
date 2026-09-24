//! Invitation admission uses a person/device identity independent of the workspace owner.
//! Persisted material is custody, not evidence of current workspace membership.
use ed25519_dalek::SigningKey;
use qos_peer_state::join::{self, JoinChallenge, JoinCompleteRequest, VerifiedJoin};
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

const MAX_INVITATION: usize = 65_536;
const MAX_MATERIAL: usize = 1_048_576;
const MAX_COMPLETION: usize = 262_144;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterialV3 {
    version: u32,
    root_secret: [u8; 32],
    device_secret: [u8; 32],
    request_id: [u8; 32],
    certificate: DeviceCert,
    invitation: String,
    created_at_ns: u64,
    completion: Option<Vec<u8>>,
    completion_at_ns: Option<u64>,
    admitted: bool,
}

// Reserve the bounded MaterialV3 JSON backing and growth overlap before decode.
// This is outer identity custody, not qualification of legacy StatePeer or
// invitation verification internals. The owned material drops before credit.
const MATERIAL_CREDIT: usize = 4 * MAX_MATERIAL;
pub(crate) struct InvitedIdentity(
    MaterialV3,
    crate::peer_delivery::incoming::IncomingBudget,
    automerge::resource_limits::NativeReservation,
);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl InvitedIdentity {
    pub(crate) fn scope(text: &str) -> Result<String, String> {
        if text.is_empty() || text.len() > MAX_INVITATION {
            return Err("Invalid invitation size".into());
        }
        // This lookup key deliberately has no expiry or authority semantics. It
        // must still find custody after an invitation/challenge has expired.
        tilo_user_identity::canonical::signing_digest(
            b"qos-browser-invited-custody-locator-v3\0",
            &text,
        )
        .map(|digest| hex(&digest))
        .map_err(|error| error.to_string())
    }

    pub(crate) fn create(text: &str, label: &str, now: u64) -> Result<Vec<u8>, String> {
        Self::scope(text)?;
        if label.trim().is_empty() || label.len() > 128 || now == 0 {
            return Err("Invalid invited identity label/time".into());
        }
        let join =
            join::decode_and_verify_invitation_text(text, None, &RevocationList::new(), now)?;
        if join.grant().recipient_user_public_key.is_some() {
            return Err("A new person cannot redeem a recipient-bound invitation".into());
        }
        let (root, _) = tilo_user_identity::UserIdentity::generate(label, now);
        let (device, _) = tilo_user_identity::DeviceIdentity::generate(label, now);
        let (request, _) = tilo_user_identity::DeviceIdentity::generate("Join request", now);
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).map_err(|e| e.to_string())?;
        Self::encode_material(&MaterialV3 {
            version: 3,
            root_secret: root.to_bytes(),
            device_secret: device.to_bytes(),
            request_id: request.to_bytes(),
            certificate,
            invitation: text.into(),
            created_at_ns: now,
            completion: None,
            completion_at_ns: None,
            admitted: false,
        })
    }

    pub(crate) fn read_with_budget(
        bytes: &[u8],
        now: u64,
        budget: &crate::peer_delivery::incoming::IncomingBudget,
    ) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_MATERIAL {
            return Err("Invalid invited material size".into());
        }
        let credit = budget.reserve(MATERIAL_CREDIT)?;
        let material: MaterialV3 =
            serde_json::from_slice(bytes).map_err(|_| "Invalid invited material")?;
        if material.version != 3
            || material.created_at_ns == 0
            || material.created_at_ns > now
            || material.request_id == [0; 32]
            || material.root_secret == [0; 32]
            || material.device_secret == [0; 32]
            || material.certificate.user_public_key
                != SigningKey::from_bytes(&material.root_secret)
                    .verifying_key()
                    .to_bytes()
            || material.certificate.device_public_key
                != SigningKey::from_bytes(&material.device_secret)
                    .verifying_key()
                    .to_bytes()
            || material.certificate.capabilities != DeviceCapabilities::member()
            || material.certificate.issued_at_ns != material.created_at_ns
        {
            return Err("Invited identity custody mismatch".into());
        }
        Self::scope(&material.invitation)?;
        tilo_user_identity::verify_device_cert_at(&material.certificate, now)
            .map_err(|e| e.to_string())?;
        let identity = Self(material, budget.clone(), credit);
        // Historical verification recovers only pinned invitation/identity. A
        // live first redemption separately enforces the current invitation time.
        identity.invitation(identity.0.created_at_ns)?;
        match (&identity.0.completion, identity.0.completion_at_ns) {
            (Some(bytes), Some(at))
                if at >= identity.0.created_at_ns && at <= now && bytes.len() <= MAX_COMPLETION =>
            {
                let charge = crate::peer_onboarding::preflight_completion(bytes)?;
                let _completion_credit = budget.reserve(charge)?;
                let complete: JoinCompleteRequest =
                    postcard::from_bytes(bytes).map_err(|_| "Invalid retained completion")?;
                identity.validate_challenge(&complete.proof.challenge, at)?;
                join::verify_join_proof(
                    &complete.proof,
                    &complete.proof.challenge,
                    identity.certificate(),
                    &RevocationList::new(),
                    at,
                )?;
            }
            (None, None) if !identity.0.admitted => {}
            _ => return Err("Invalid retained completion checkpoint".into()),
        }
        Ok(identity)
    }

    // Stateless custody inspection has no existing Peer. Actual attempts use
    // read_with_budget and carry that same allowance into the final owner.
    pub(crate) fn read(bytes: &[u8], now: u64) -> Result<Self, String> {
        let budget = crate::peer_delivery::incoming::IncomingBudget::new(64 * 1024 * 1024)?;
        Self::read_with_budget(bytes, now, &budget)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        Self::encode_material(&self.0)
    }

    fn encode_material(material: &MaterialV3) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(material).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_MATERIAL {
            return Err("Invited material exceeds bound".into());
        }
        Ok(bytes)
    }

    pub(crate) fn certificate(&self) -> &DeviceCert {
        &self.0.certificate
    }
    pub(crate) fn device_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.0.device_secret)
    }
    pub(crate) fn request_id(&self) -> [u8; 32] {
        self.0.request_id
    }
    pub(crate) fn completion(&self) -> Option<&[u8]> {
        self.0.completion.as_deref()
    }
    pub(crate) fn admitted(&self) -> bool {
        self.0.admitted
    }

    pub(crate) fn public(&self) -> Result<String, String> {
        let scope = Self::scope(&self.0.invitation)?;
        let binding = tilo_user_identity::canonical::signing_digest(
            b"qos-browser-invited-custody-binding-v3\0",
            &(
                &scope,
                &self.0.certificate,
                self.0.request_id,
                self.0.created_at_ns,
            ),
        )
        .map_err(|e| e.to_string())?;
        serde_json::to_string(&serde_json::json!({
            "scope": scope, "device_public_key": hex(&self.0.certificate.device_public_key),
            "binding": hex(&binding),
        }))
        .map_err(|e| e.to_string())
    }

    pub(crate) fn invitation(&self, now: u64) -> Result<VerifiedJoin, String> {
        join::decode_and_verify_invitation_text(
            &self.0.invitation,
            Some(self.0.certificate.user_public_key),
            &RevocationList::new(),
            now,
        )
    }

    pub(crate) fn retained_invitation(&self) -> Result<VerifiedJoin, String> {
        self.invitation(self.0.created_at_ns)
    }

    pub(crate) fn selected_delivery_pin(&self) -> Result<Option<(DeviceCert, String)>, String> {
        let verified = self.retained_invitation()?;
        let mut locators =
            verified.grant().transport.iter().filter(|hint| {
                hint.kind == tilo_relay::workspace_join::TRANSPORT_DELIVERY_ORIGIN_V1
            });
        let Some(hint) = locators.next() else {
            return Ok(None);
        };
        if locators.next().is_some() {
            return Err("Ambiguous invitation delivery origins".into());
        }
        // The certificate is selected from the verified outer invitation, never
        // from the subsequently discovered descriptor. Current appointment is
        // checked against admitted governance before this origin is used.
        let locator = tilo_relay::workspace_join::DeliveryOriginLocatorV1::from_transport_hint(
            hint, &hint.host,
        )
        .map_err(|error| error.to_string())?;
        Ok(Some((hint.host.clone(), locator.origin)))
    }

    pub(crate) fn delivery_pin(
        &self,
        state: &qos_peer_state::StatePeer,
        now: u64,
    ) -> Result<Option<(DeviceCert, String)>, String> {
        let Some((host, origin)) = self.selected_delivery_pin()? else {
            return Ok(None);
        };
        let pinned = self.retained_invitation()?;
        let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now);
        tilo_user_identity::verify_device_cert_at(&host, now).map_err(|error| error.to_string())?;
        if state.authority() != &pinned.grant().authority
            || state.room() != pinned.grant().room
            || !host.capabilities.can_act_as_host
            || !state.certificates().contains(&host)
            || state.permissions(host.device_public_key, time).is_err()
        {
            return Err("Invitation delivery host is not currently appointed".into());
        }
        Ok(Some((host, origin)))
    }

    fn validate_challenge(&self, challenge: &JoinChallenge, now: u64) -> Result<(), String> {
        let expected = JoinChallenge::new(
            &self.invitation(now)?,
            self.certificate(),
            self.request_id(),
            challenge.nonce,
            challenge.expires_at_ns,
            now,
        )?;
        if expected != *challenge || challenge.nonce == [0; 32] {
            return Err("Join challenge does not match retained intent".into());
        }
        Ok(())
    }

    pub(crate) fn checkpoint_challenge(
        &mut self,
        challenge: JoinChallenge,
        now: u64,
    ) -> Result<Vec<u8>, String> {
        if let Some(bytes) = self.completion() {
            let charge = crate::peer_onboarding::preflight_completion(bytes)?;
            let _completion_credit = self.1.reserve(charge)?;
            let previous: JoinCompleteRequest =
                postcard::from_bytes(bytes).map_err(|_| "Invalid retained completion")?;
            if previous.proof.challenge != challenge {
                return Err("Redemption completion is already fixed".into());
            }
            return self.encode();
        }
        // Covers the transient signature encoding and owned completion backing;
        // persistent completion bytes then remain under MATERIAL_CREDIT.
        let _completion_credit = self.1.reserve(3 * MAX_COMPLETION)?;
        self.validate_challenge(&challenge, now)?;
        let complete = JoinCompleteRequest {
            proof: join::sign_join_proof(challenge, &self.device_key())?,
        };
        let bytes = postcard::to_stdvec(&complete).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_COMPLETION {
            return Err("Join completion exceeds bound".into());
        }
        self.0.completion = Some(bytes);
        self.0.completion_at_ns = Some(now);
        self.encode()
    }

    pub(crate) fn admit_bootstrap(
        &mut self,
        bootstrap: &join::JoinBootstrap,
        now: u64,
    ) -> Result<qos_peer_state::StatePeer, String> {
        let pinned = self.retained_invitation()?;
        if self.completion().is_none()
            || bootstrap.version != 1
            || bootstrap.invite_id != pinned.invite_id()
            || bootstrap.device_cert != *self.certificate()
            || bootstrap.authority != pinned.grant().authority
            || bootstrap.room != pinned.grant().room
            || bootstrap.workspace_id != pinned.grant().workspace_id
            || bootstrap.generation == 0
            || bootstrap.governance_messages.len() > 4096
            || bootstrap
                .governance_messages
                .iter()
                .try_fold(0usize, |sum, bytes| sum.checked_add(bytes.len()))
                .is_none_or(|sum| sum > 16 * 1024 * 1024)
        {
            return Err("Workspace bootstrap does not match retained invitation custody".into());
        }
        let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now);
        let initial = qos_peer_state::StatePeer::new(
            &bootstrap.room,
            bootstrap.authority.clone(),
            vec![bootstrap.authority.clone()],
            RevocationList::new(),
            time,
        )?;
        // This private owner consumes only the pinned host's authenticated
        // Complete exchange. Native bootstrap publications are retained signed
        // bytes, not live writes: governor and original member publications
        // enter one bounded native cut through StatePeer's validated incoming
        // path. Current device/role checks still follow below.
        let mut cut =
            crate::peer_delivery::admission::CutAdmission::begin(&initial, self.1.clone(), time)?;
        for bytes in &bootstrap.governance_messages {
            cut.push_retained(bytes, time)
                .map_err(|error| format!("Invalid bootstrap governance message: {error}"))?;
        }
        let state = cut.finish(time)?;
        if !state.certificates().contains(self.certificate()) {
            return Err("Invited device not admitted by signed governance".into());
        }
        let (role, _, _) = state.permissions(self.certificate().device_public_key, time)?;
        let expected = match bootstrap.role {
            join::MemberRole::Editor => collab_types::Role::Editor,
            join::MemberRole::Commenter => collab_types::Role::Commenter,
            join::MemberRole::Viewer => collab_types::Role::Viewer,
        };
        if role != expected {
            return Err("Workspace bootstrap role differs from signed governance".into());
        }
        self.0.admitted = true;
        Ok(state)
    }
}
#[cfg(test)]
#[path = "peer_invitation/tests.rs"]
mod tests;
