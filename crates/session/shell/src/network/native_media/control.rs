//! Closed media intent operations; callers cannot supply a signing digest.
use super::*;
use media_wire::{
    ChallengeTranscript, ControlCommand, ControlReply, ControlResult, GrantOperation, GrantRequest,
    MediaContext, MediaDirections, SignedChallenge, SignedGrant,
};

/// The media host signs with its wall clock; this device's clock may trail it
/// (another machine, or a drifting local clock). Accept signed issuance up to
/// this far ahead, as live grants do. Lifetimes stay bounded by the signed
/// duration, so skew never extends a grant.
pub(super) const CLOCK_SKEW_NS: u64 = 5_000_000_000;

pub(super) struct Participation {
    pub id: u32,
    pub request: GrantRequest,
    pub context: Option<MediaContext>,
    pub grant: Option<SignedGrant>,
    pub relays: BTreeMap<u32, media_wire::RelayProofChallenge>,
    pub relay_expires: BTreeMap<u32, u64>,
    pub leaving: bool,
    pub presence_expires_at_ns: u64,
}
pub(super) enum ControlStage {
    GrantChallenge(GrantOperation),
    GrantComplete(GrantOperation),
    RelayChallenge,
    RefreshChallenge(u32),
    RefreshComplete(u32, media_wire::RefreshChallenge),
    Close(u32),
}
/// Safe operational projection. Grant bytes remain inside Rust custody.
pub(in super::super) struct GrantView {
    pub handle: u32,
    pub expires_in_ns: u64,
    pub presence_in_ns: u64,
    pub directions: MediaDirections,
    pub namespace: media_wire::MediaNamespace,
    pub inventory: media_wire::PublisherInventory,
}
pub(in super::super) struct RelayView {
    pub handle: u32,
    pub carrier: u32,
    pub endpoint: String,
    pub setup: Vec<u8>,
    pub namespace: media_wire::MediaNamespace,
    pub inventory: media_wire::PublisherInventory,
}
pub(in super::super) struct PresenceView {
    pub expires_in_ns: u64,
    pub inventory: media_wire::PublisherInventory,
}

pub(super) fn policy(
    view: &CurrentView<'_>,
    request: &GrantRequest,
) -> Result<(u64, u64, MediaDirections), String> {
    use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
    use qos_peer_state::room::media::{CallPhaseV1, parse_call_id};
    if request.device != view.device.device_public_key || request.room != view.peer.room() {
        return Err("media intent custody mismatch".into());
    }
    view.peer
        .permissions(view.governor.device_public_key, time(view.now_ns))?;
    let (role, caps, person) = view.peer.permissions(request.device, time(view.now_ns))?;
    let actor = Actor::person(Some(role), &person).in_room(request.room.clone());
    for (requested, verb) in [
        (request.directions.publish, Verb::Write),
        (request.directions.subscribe, Verb::Read),
    ] {
        if requested
            && decide_capability(
                &actor,
                &ResourceRef::Room(request.room.clone()),
                verb,
                &caps,
                Some(view.now_ns / 1_000_000_000),
            ) != CapDecision::Allow
        {
            return Err("media room direction denied".into());
        }
    }
    let policy = view
        .peer
        .validated_media_policy()?
        .ok_or("governed media unavailable")?;
    let call = policy
        .calls
        .get(&parse_call_id(&request.call_id).map_err(fail)?)
        .ok_or("unknown governed call")?;
    let denied = call.denied.get(&view.device.user_public_key);
    let directions = MediaDirections {
        publish: call.allowed.publish && !denied.is_some_and(|d| d.publish),
        subscribe: call.allowed.subscribe && !denied.is_some_and(|d| d.subscribe),
    };
    if call.phase != CallPhaseV1::Active
        || (request.directions.publish && !directions.publish)
        || (request.directions.subscribe && !directions.subscribe)
    {
        return Err("governed call direction denied".into());
    }
    Ok((call.epoch, policy.authorization_revision, directions))
}
fn verify(key: [u8; 32], digest: [u8; 32], signature: [[u8; 32]; 2]) -> Result<(), String> {
    ed25519_dalek::VerifyingKey::from_bytes(&key)
        .map_err(fail)?
        .verify_strict(
            &digest,
            &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(signature)),
        )
        .map_err(fail)
}
impl NativeControl {
    fn control_request(
        &mut self,
        view: &OwnerView<'_>,
        command: ControlCommand,
        stage: ControlStage,
    ) -> Result<PreparedRequest, String> {
        self.check(view)?;
        let session = self
            .session
            .as_ref()
            .ok_or("media HTTP handshake required")?;
        self.verify_host(view, &session.proof, session.nonce)?;
        let part = self
            .participation
            .as_ref()
            .ok_or("no media participation")?;
        let intent = part.id;
        let target = command.target();
        let body = media_wire::ControlRequest {
            version: 1,
            command,
        }
        .encode()
        .map_err(fail)?;
        let expires = view
            .now_ns
            .checked_add(60_000_000_000)
            .ok_or("media proof time overflow")?
            .min(session.proof.transcript.expires_at_ns);
        let proof = http_proof::RequestProof::sign(
            session.proof.transcript.session_id,
            session.digest,
            &http_proof::Request::new("POST", target, &body).map_err(fail)?,
            view.now_ns,
            expires,
            view.key,
        )
        .map_err(fail)?
        .encode()
        .map_err(fail)?;
        self.request(
            target,
            body,
            Some(proof),
            Pending::Control { intent, stage },
        )
    }
    pub fn start_join(
        &mut self,
        view: &OwnerView<'_>,
        call_id: &str,
        directions: MediaDirections,
    ) -> Result<PreparedRequest, String> {
        self.check(view)?;
        if self.participation.is_some() || self.session.is_none() {
            return Err("media join unavailable".into());
        }
        let mut request = GrantRequest {
            device: view.device.device_public_key,
            room: view.peer.room().into(),
            call_id: call_id.into(),
            participation_id: media_wire::fresh_nonce().map_err(fail)?,
            generation: 0,
            directions,
        };
        request.validate(true).map_err(fail)?;
        // Reserve only directions currently permitted by both room and call.
        // An explicitly listening-only request never asks for publication.
        let permitted = |publish, subscribe| {
            let mut candidate = request.clone();
            candidate.directions = MediaDirections { publish, subscribe };
            policy(&view.current_view(), &candidate).is_ok()
        };
        request.directions = MediaDirections {
            publish: directions.publish && permitted(true, false),
            subscribe: directions.subscribe && permitted(false, true),
        };
        request.validate(true).map_err(fail)?;
        policy(&view.current_view(), &request)?;
        let id = self
            .next
            .checked_add(1)
            .ok_or("media intent counter exhausted")?;
        self.participation = Some(Participation {
            id,
            request: request.clone(),
            context: None,
            grant: None,
            relays: BTreeMap::new(),
            relay_expires: BTreeMap::new(),
            leaving: false,
            presence_expires_at_ns: 0,
        });
        let result = self.control_request(
            view,
            ControlCommand::GrantChallenge {
                request,
                operation: GrantOperation::Join,
            },
            ControlStage::GrantChallenge(GrantOperation::Join),
        );
        if result.is_err() {
            self.participation = None;
        }
        result
    }
    fn part(&self, handle: u32) -> Result<&Participation, String> {
        self.participation
            .as_ref()
            .filter(|p| p.id == handle && !p.leaving)
            .ok_or("stale media intent".into())
    }
    pub fn renew(&mut self, view: &OwnerView<'_>, handle: u32) -> Result<PreparedRequest, String> {
        let request = self.part(handle)?.request.clone();
        policy(&view.current_view(), &request)?;
        self.control_request(
            view,
            ControlCommand::GrantChallenge {
                request,
                operation: GrantOperation::Renew,
            },
            ControlStage::GrantChallenge(GrantOperation::Renew),
        )
    }
    pub fn relay_challenge(
        &mut self,
        view: &OwnerView<'_>,
        handle: u32,
    ) -> Result<PreparedRequest, String> {
        let part = self.part(handle)?;
        policy(&view.current_view(), &part.request)?;
        let grant = part
            .grant
            .as_ref()
            .filter(|grant| grant.metadata.expires_at_ns > view.now_ns)
            .ok_or("live media grant required")?
            .encode()
            .map_err(fail)?;
        self.control_request(
            view,
            ControlCommand::RelayChallenge { grant },
            ControlStage::RelayChallenge,
        )
    }
    pub fn refresh(
        &mut self,
        view: &OwnerView<'_>,
        handle: u32,
        carrier: u32,
    ) -> Result<PreparedRequest, String> {
        let part = self.part(handle)?;
        policy(&view.current_view(), &part.request)?;
        let session_id = part
            .relays
            .get(&carrier)
            .ok_or("stale media carrier")?
            .session_id;
        let grant = part
            .grant
            .as_ref()
            .filter(|grant| grant.metadata.expires_at_ns > view.now_ns)
            .ok_or("live media grant required")?
            .encode()
            .map_err(fail)?;
        self.control_request(
            view,
            ControlCommand::RefreshChallenge { session_id, grant },
            ControlStage::RefreshChallenge(carrier),
        )
    }
    pub fn heartbeat(
        &mut self,
        view: &OwnerView<'_>,
        handle: u32,
    ) -> Result<PreparedRequest, String> {
        let request = self.part(handle)?.request.clone();
        policy(&view.current_view(), &request)?;
        self.control_request(
            view,
            ControlCommand::GrantChallenge {
                request,
                operation: GrantOperation::Heartbeat,
            },
            ControlStage::GrantChallenge(GrantOperation::Heartbeat),
        )
    }
    pub fn close_relay(
        &mut self,
        view: &OwnerView<'_>,
        handle: u32,
        carrier: u32,
    ) -> Result<PreparedRequest, String> {
        let part = self
            .participation
            .as_ref()
            .filter(|p| p.id == handle)
            .ok_or("stale media participation")?;
        let session_id = part
            .relays
            .get(&carrier)
            .ok_or("stale media carrier")?
            .session_id;
        self.control_request(
            view,
            ControlCommand::RelayClose { session_id },
            ControlStage::Close(carrier),
        )
    }
    /// Fence publication immediately; only the exact retained Leave proof can continue.
    pub fn leave(&mut self, view: &OwnerView<'_>, handle: u32) -> Result<Step, String> {
        if !self.participation.as_ref().is_some_and(|p| p.id == handle) {
            return Ok(Step::Left);
        }
        // Older cleanup may expire remotely, but it cannot consume another live intent.
        if let Some(old) = self.ending.take() {
            self.pending
                .retain(|_, p| !matches!(p,Pending::Control{intent,..} if *intent==old.id));
        }
        self.pending
            .retain(|_, p| !matches!(p,Pending::Control{intent,..} if *intent==handle));
        let part = self.participation.as_mut().unwrap();
        part.leaving = true;
        if part.request.generation == 0 {
            self.participation = None;
            return Ok(Step::Left);
        }
        let request = part.request.clone();
        let result = self
            .control_request(
                view,
                ControlCommand::GrantChallenge {
                    request,
                    operation: GrantOperation::Leave,
                },
                ControlStage::GrantChallenge(GrantOperation::Leave),
            )
            .map(Step::Request);
        self.ending = self.participation.take();
        result
    }
    pub fn can_publish(&self, view: &CurrentView<'_>) -> Result<bool, String> {
        self.check_current(view)?;
        let Some(part) = &self.participation else {
            return Ok(false);
        };
        if part.leaving || !part.request.directions.publish {
            return Ok(false);
        }
        policy(&view.current_view(), &part.request)?;
        Ok(part
            .grant
            .as_ref()
            .is_some_and(|g| g.metadata.expires_at_ns > view.now_ns))
    }
    fn context(
        &self,
        view: &OwnerView<'_>,
        context: &MediaContext,
        reservation: bool,
    ) -> Result<(), String> {
        let part = self.participation.as_ref().ok_or("stale media intent")?;
        let mut expected = part.request.clone();
        if reservation && expected.generation == 0 {
            expected.generation = context.request.generation;
        }
        let (epoch, revision, directions) = policy(&view.current_view(), &expected)?;
        let session = self.session.as_ref().ok_or("media HTTP session missing")?;
        let binding = &session.proof.transcript.binding;
        if context.request != expected
            || context.request.generation == 0
            || context.descriptor_digest != self.descriptor.descriptor_digest()
            || context.delivery_session_id != session.proof.transcript.session_id
            || context.delivery_boot != binding.server_boot_id
            || context.origin != self.descriptor.origin()
            || context.workspace != view.workspace
            || context.room != view.peer.room()
            || context.root != view.device.user_public_key
            || context.delivery_host != view.host.device_public_key
            || context.media_host != view.governor.device_public_key
            || context.epoch != epoch
            || context.membership_generation != revision
            || context.directions != directions
            || context.issued_at_ns > view.now_ns.saturating_add(CLOCK_SKEW_NS)
            || context.expires_at_ns <= view.now_ns
        {
            return Err("signed media context does not match current custody".into());
        }
        let call = tilo_user_identity::canonical::signing_digest(
            media_wire::MEDIA_PROOF_DOMAIN,
            &(
                3_u8,
                context.media_boot,
                &expected.room,
                &expected.call_id,
                context.epoch,
            ),
        )
        .map_err(fail)?;
        let own = tilo_user_identity::canonical::signing_digest(
            media_wire::MEDIA_PROOF_DOMAIN,
            &(
                4_u8,
                call,
                view.device.user_public_key,
                expected.device,
                expected.participation_id,
                expected.generation,
            ),
        )
        .map_err(fail)?;
        if context
            .inventory
            .namespaces
            .iter()
            .any(|n| n.call != call || n.publisher == own)
        {
            return Err("media inventory exceeds exact remote call scope".into());
        }
        if let Some(previous) = &part.context {
            if context.media_boot != previous.media_boot
                || context.epoch != previous.epoch
                || context.inventory.revision < previous.inventory.revision
                || (context.inventory.revision == previous.inventory.revision
                    && context.inventory != previous.inventory)
            {
                return Err("media boot or inventory continuity changed".into());
            }
        }
        Ok(())
    }
    fn challenge(
        &self,
        view: &OwnerView<'_>,
        value: &SignedChallenge,
        reservation: bool,
    ) -> Result<(), String> {
        value.validate().map_err(fail)?;
        verify(
            view.governor.device_public_key,
            value.signing_digest().map_err(fail)?,
            value.signature,
        )?;
        self.context(view, &value.context, reservation)?;
        let audience = match &value.transcript {
            ChallengeTranscript::Native(v) => &v.audience,
            ChallengeTranscript::Relay(v) => &v.audience,
            ChallengeTranscript::Refresh(v) => &v.audience,
        };
        if audience != &format!("{}/moq/v21/voice", self.descriptor.origin()) {
            return Err("media audience mismatch".into());
        }
        Ok(())
    }
    pub(super) fn grant_view(&self, view: &CurrentView<'_>) -> Result<GrantView, String> {
        let part = self
            .participation
            .as_ref()
            .ok_or("media participation missing")?;
        let grant = part.grant.as_ref().ok_or("media grant missing")?;
        let expires_in_ns = grant
            .metadata
            .expires_at_ns
            .checked_sub(view.now_ns)
            // Bounded by the signed lifetime, even when the host clock leads.
            .map(|v| {
                v.min(
                    grant
                        .metadata
                        .expires_at_ns
                        .saturating_sub(grant.metadata.issued_at_ns),
                )
            })
            .filter(|v| *v > 0)
            .ok_or("expired media grant")?;
        let presence_in_ns = part
            .presence_expires_at_ns
            .checked_sub(view.now_ns)
            .filter(|v| *v > 0)
            .ok_or("expired media presence")?;
        Ok(GrantView {
            handle: part.id,
            expires_in_ns,
            presence_in_ns,
            directions: part.request.directions,
            namespace: grant.metadata.namespace,
            inventory: part
                .context
                .as_ref()
                .ok_or("media context missing")?
                .inventory
                .clone(),
        })
    }
    pub(super) fn advance_control(
        &mut self,
        view: &OwnerView<'_>,
        intent: u32,
        stage: ControlStage,
        bytes: &[u8],
    ) -> Result<Step, String> {
        let part = self
            .participation
            .as_ref()
            .filter(|p| p.id == intent)
            .ok_or("stale media completion")?;
        if part.leaving
            && !matches!(
                stage,
                ControlStage::GrantChallenge(GrantOperation::Leave)
                    | ControlStage::GrantComplete(GrantOperation::Leave)
                    | ControlStage::Close(_)
            )
        {
            return Err("media participation ended".into());
        }
        let result = ControlReply::decode(bytes).map_err(fail)?.result;
        if let ControlResult::Error(code) = result {
            return Err(format!("media-{code:?}"));
        }
        match (stage, result) {
            (ControlStage::GrantChallenge(operation), ControlResult::Challenge(challenge)) => {
                self.challenge(view, &challenge, operation == GrantOperation::Join)?;
                let ChallengeTranscript::Native(native) = &challenge.transcript else {
                    return Err("wrong media proof purpose".into());
                };
                if native.operation != operation {
                    return Err("wrong media operation".into());
                }
                let signature = media_wire::split_signature(
                    view.key
                        .sign(&challenge.transcript.signing_digest().map_err(fail)?)
                        .to_bytes(),
                );
                let part = self.participation.as_mut().unwrap();
                part.request = native.request.clone();
                part.context = Some(challenge.context.clone());
                self.control_request(
                    view,
                    ControlCommand::GrantComplete {
                        nonce: native.nonce,
                        signature,
                    },
                    ControlStage::GrantComplete(operation),
                )
                .map(Step::Request)
            }
            (ControlStage::GrantComplete(operation), ControlResult::Granted { grant })
                if matches!(operation, GrantOperation::Join | GrantOperation::Renew) =>
            {
                let grant = SignedGrant::decode(&grant).map_err(fail)?;
                verify(
                    view.governor.device_public_key,
                    grant.signing_digest().map_err(fail)?,
                    grant.signature,
                )?;
                let part = self.participation.as_ref().unwrap();
                let c = part
                    .context
                    .as_ref()
                    .ok_or("missing signed media context")?;
                self.context(view, c, false)?;
                let m = &grant.metadata;
                if m.request != part.request
                    || m.root != view.device.user_public_key
                    || m.host != c.media_host
                    || m.host_boot != c.media_boot
                    || m.epoch != c.epoch
                    || m.membership_generation != c.membership_generation
                    || m.audience != format!("{}/moq/v21/voice", self.descriptor.origin())
                    || m.issued_at_ns < c.issued_at_ns
                    || m.issued_at_ns > view.now_ns.saturating_add(CLOCK_SKEW_NS)
                    || m.expires_at_ns <= view.now_ns
                {
                    return Err("media grant context mismatch".into());
                }
                let call = tilo_user_identity::canonical::signing_digest(
                    media_wire::MEDIA_PROOF_DOMAIN,
                    &(
                        3_u8,
                        m.host_boot,
                        &m.request.room,
                        &m.request.call_id,
                        m.epoch,
                    ),
                )
                .map_err(fail)?;
                let publisher = tilo_user_identity::canonical::signing_digest(
                    media_wire::MEDIA_PROOF_DOMAIN,
                    &(
                        4_u8,
                        call,
                        m.root,
                        m.request.device,
                        m.request.participation_id,
                        m.request.generation,
                    ),
                )
                .map_err(fail)?;
                if m.namespace != (media_wire::MediaNamespace { call, publisher }) {
                    return Err("media namespace derivation mismatch".into());
                }
                let part = self.participation.as_mut().unwrap();
                if operation == GrantOperation::Join {
                    part.presence_expires_at_ns = m
                        .issued_at_ns
                        .min(view.now_ns)
                        .checked_add(15_000_000_000)
                        .ok_or("media presence time overflow")?
                        .min(m.expires_at_ns);
                }
                part.grant = Some(grant);
                self.grant_view(&view.current_view()).map(Step::Grant)
            }
            (ControlStage::GrantComplete(GrantOperation::Leave), ControlResult::Left) => {
                self.participation = None;
                Ok(Step::Left)
            }
            (ControlStage::RelayChallenge, ControlResult::Challenge(challenge)) => {
                self.challenge(view, &challenge, false)?;
                let ChallengeTranscript::Relay(relay) = &challenge.transcript else {
                    return Err("wrong relay proof purpose".into());
                };
                let part = self.participation.as_ref().unwrap();
                let grant = part
                    .grant
                    .as_ref()
                    .filter(|grant| grant.metadata.expires_at_ns > view.now_ns)
                    .ok_or("live grant missing")?;
                if relay.grant_digest
                    != media_wire::grant_digest(&grant.encode().map_err(fail)?).map_err(fail)?
                {
                    return Err("relay grant mismatch".into());
                }
                let setup = media_wire::RelaySubmission {
                    version: 1,
                    session: relay.session_id,
                    nonce: relay.nonce,
                    signature: media_wire::split_signature(
                        view.key
                            .sign(&challenge.transcript.signing_digest().map_err(fail)?)
                            .to_bytes(),
                    ),
                }
                .encode()
                .map_err(fail)?;
                if part.relays.len() >= 2 {
                    return Err("media carrier capacity".into());
                }
                let carrier = self
                    .next
                    .checked_add(1)
                    .ok_or("media carrier counter exhausted")?;
                self.next = carrier;
                let result = RelayView {
                    handle: part.id,
                    carrier,
                    endpoint: relay.audience.clone(),
                    setup,
                    namespace: grant.metadata.namespace,
                    inventory: challenge.context.inventory.clone(),
                };
                let expiry = grant.metadata.expires_at_ns;
                let part = self.participation.as_mut().unwrap();
                part.context = Some(challenge.context);
                part.relay_expires.insert(carrier, expiry);
                part.relays.insert(carrier, relay.clone());
                Ok(Step::Relay(result))
            }
            (ControlStage::RefreshChallenge(carrier), ControlResult::Challenge(challenge)) => {
                self.challenge(view, &challenge, false)?;
                let ChallengeTranscript::Refresh(refresh) = &challenge.transcript else {
                    return Err("wrong refresh purpose".into());
                };
                let part = self.participation.as_ref().unwrap();
                let relay = part.relays.get(&carrier).ok_or("stale media carrier")?;
                let grant = part
                    .grant
                    .as_ref()
                    .filter(|grant| grant.metadata.expires_at_ns > view.now_ns)
                    .ok_or("live grant missing")?;
                if refresh.session_id != relay.session_id
                    || refresh.authorization != relay.nonce
                    || refresh.grant_digest
                        != media_wire::grant_digest(&grant.encode().map_err(fail)?).map_err(fail)?
                {
                    return Err("refresh binding mismatch".into());
                }
                let signature = media_wire::split_signature(
                    view.key
                        .sign(&challenge.transcript.signing_digest().map_err(fail)?)
                        .to_bytes(),
                );
                self.participation.as_mut().unwrap().context = Some(challenge.context);
                self.control_request(
                    view,
                    ControlCommand::RefreshComplete {
                        session_id: refresh.session_id,
                        nonce: refresh.nonce,
                        signature,
                    },
                    ControlStage::RefreshComplete(carrier, refresh.clone()),
                )
                .map(Step::Request)
            }
            (
                ControlStage::RefreshComplete(carrier, refresh),
                ControlResult::Refreshed {
                    session_id,
                    expires_at_ns,
                },
            ) => {
                let part = self.participation.as_mut().unwrap();
                if session_id != refresh.session_id
                    || expires_at_ns
                        != part
                            .grant
                            .as_ref()
                            .ok_or("grant missing")?
                            .metadata
                            .expires_at_ns
                    || expires_at_ns <= view.now_ns
                {
                    return Err("refresh receipt mismatch".into());
                }
                let relay = part.relays.get_mut(&carrier).ok_or("stale media carrier")?;
                relay.nonce = refresh.nonce;
                relay.grant_digest = refresh.grant_digest;
                part.relay_expires.insert(carrier, expires_at_ns);
                Ok(Step::Refreshed)
            }
            (
                ControlStage::GrantComplete(GrantOperation::Heartbeat),
                ControlResult::Present(presence),
            ) => {
                verify(
                    view.governor.device_public_key,
                    presence.signing_digest().map_err(fail)?,
                    presence.signature,
                )?;
                self.context(view, &presence.context, false)?;
                let part = self.participation.as_mut().unwrap();
                if presence.context.expires_at_ns
                    > part
                        .grant
                        .as_ref()
                        .ok_or("grant missing")?
                        .metadata
                        .expires_at_ns
                {
                    return Err("presence exceeds grant".into());
                }
                part.presence_expires_at_ns = presence.context.expires_at_ns;
                let result = PresenceView {
                    // Bounded by the signed lease, even when the host clock leads.
                    expires_in_ns: (presence.context.expires_at_ns - view.now_ns).min(
                        presence
                            .context
                            .expires_at_ns
                            .saturating_sub(presence.context.issued_at_ns),
                    ),
                    inventory: presence.context.inventory.clone(),
                };
                part.context = Some(presence.context);
                Ok(Step::Presence(result))
            }
            (ControlStage::Close(carrier), ControlResult::Closed) => {
                if let Some(part) = &mut self.participation {
                    part.relays.remove(&carrier);
                    part.relay_expires.remove(&carrier);
                }
                Ok(Step::RelayClosed)
            }
            _ => Err("unexpected media control response".into()),
        }
    }
}

#[cfg(test)]
#[path = "control_clock_tests.rs"]
mod clock_tests;
