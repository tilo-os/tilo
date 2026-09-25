//! Caller-bound native control. Only an admitted exact HTTP request reaches signing.
use super::*;
use tilo_relay::delivery_v3::{
    http_proof::VerifiedRequest,
    media_wire::{
        ChallengeTranscript, ControlCommand, ControlRequest, ControlResult, MediaContext,
        PublisherInventory, SignedChallenge, SignedPresence, split_signature,
    },
};
fn caller(request: &VerifiedRequest<'_>, intent: &GrantRequest) -> Result<(), MediaError> {
    if request.session().device_certificate().device_public_key != intent.device
        || request.session().binding().device_root
            != request.session().device_certificate().user_public_key
    {
        return Err(MediaError::Unauthorized);
    }
    Ok(())
}
impl MediaService {
    pub(crate) fn control(
        &self,
        owner: &SessionOwner,
        request: &VerifiedRequest<'_>,
    ) -> Result<ControlResult, MediaError> {
        if !owner.permits_publication()
            || owner.workspace_id() != self.0.workspace
            || request.session().host_certificate() != owner.host_certificate()
            || owner.identity.native.verifying_key().to_bytes() != self.0.host
        {
            return Err(MediaError::Unauthorized);
        }
        match self.with_current_patient(|turn| {
            if turn.peer().authority() != owner.peer().authority()
                || turn.peer().room() != owner.peer().room()
            {
                return Err(MediaError::Unauthorized);
            }
            self.control_current(turn, request, |input| {
                Ok(owner.identity.native.sign(input.digest()).to_bytes())
            })
        }) {
            MediaObservation::Current(result) => result,
            MediaObservation::Busy => {
                tracing::debug!(reason = "owner-contended", "media control unavailable");
                Err(MediaError::Unauthorized)
            }
            MediaObservation::Denied => Err(MediaError::Unauthorized),
        }
    }
}
impl<S: MediaAuthoritySource> MediaService<S> {
    /// The original registry and this control operation must share the actual
    /// owner turn. This method never reacquires that owner's lock or retains
    /// its signer/current peer after returning.
    pub fn control_in(
        &self,
        turn: &S::Turn<'_>,
        context: &tilo_relay::delivery_v3::auth::VerificationContext<'_>,
        request: &VerifiedRequest<'_>,
        sign: impl FnMut(MediaSigningInput) -> Result<[u8; 64], MediaError>,
    ) -> Result<ControlResult, MediaError> {
        if context.now_ns != turn.now_ns() || !self.0.matches(turn) {
            return Err(MediaError::Unauthorized);
        }
        request
            .session()
            .revalidate(context)
            .map_err(|_| MediaError::Unauthorized)?;
        self.control_current(turn, request, sign)
    }
    fn control_current(
        &self,
        turn: &S::Turn<'_>,
        request: &VerifiedRequest<'_>,
        mut sign: impl FnMut(MediaSigningInput) -> Result<[u8; 64], MediaError>,
    ) -> Result<ControlResult, MediaError> {
        let wire = ControlRequest::decode(request.request().body())
            .map_err(|_| MediaError::InvalidRequest)?;
        if request.request().method() != tilo_relay::delivery_v3::http_proof::Method::Post
            || request.request().target() != crate::media_control::route(&wire.command).path()
        {
            return Err(MediaError::InvalidRequest);
        }
        let binding = request.session().binding();
        if binding.workspace != turn.workspace()
            || binding.workspace_root != turn.peer().authority().user_public_key
            || format!("{}/moq/v21/voice", binding.origin) != self.0.audience
        {
            return Err(MediaError::Unauthorized);
        }
        self.turn_in(turn, |peer, policy, core, now| match wire.command {
            ControlCommand::GrantChallenge {
                request: mut intent,
                operation,
            } => {
                caller(request, &intent)?;
                if operation == GrantOperation::Join && intent.generation == 0 {
                    intent = core.authority.reserve_join(peer, policy, intent, now)?;
                }
                let challenge =
                    core.authority
                        .challenge(peer, policy, intent.clone(), operation, now)?;
                signed_challenge(
                    self.0.host,
                    &mut sign,
                    request,
                    peer,
                    policy,
                    core,
                    intent,
                    challenge,
                    now,
                )
            }
            ControlCommand::GrantComplete { nonce, signature } => {
                let intent = core.authority.pending_control_request(nonce)?.clone();
                caller(request, &intent)?;
                match core.authority.complete(
                    peer,
                    policy,
                    nonce,
                    tilo_relay::delivery_v3::media_wire::join_signature(signature),
                    now,
                    |digest| sign(MediaSigningInput::new(*digest)),
                )? {
                    GrantCompletion::Granted(grant) => Ok(ControlResult::Granted {
                        grant: grant.as_bytes().to_vec(),
                    }),
                    GrantCompletion::Left => Ok(ControlResult::Left),
                    GrantCompletion::Present { expires_at_ns } => signed_presence(
                        self.0.host,
                        &mut sign,
                        request,
                        peer,
                        policy,
                        core,
                        intent,
                        now,
                        expires_at_ns,
                    ),
                }
            }
            ControlCommand::RelayChallenge { grant } => {
                let metadata = core.authority.control_grant(peer, policy, &grant, now)?;
                caller(request, &metadata.request)?;
                if metadata.root != binding.device_root {
                    return Err(MediaError::Unauthorized);
                }
                let mut id = [0; 32];
                rand::rngs::OsRng
                    .try_fill_bytes(&mut id)
                    .map_err(|_| MediaError::EntropyUnavailable)?;
                let challenge = core
                    .authority
                    .relay_challenge(peer, policy, &grant, id, now)?;
                signed_challenge(
                    self.0.host,
                    &mut sign,
                    request,
                    peer,
                    policy,
                    core,
                    metadata.request,
                    challenge,
                    now,
                )
            }
            ControlCommand::RefreshChallenge { session_id, grant } => {
                let slot = core
                    .sessions
                    .get(&session_id)
                    .ok_or(MediaError::StaleGeneration)?;
                caller(request, &slot.session.metadata().request)?;
                if slot.session.metadata().root != binding.device_root {
                    return Err(MediaError::Unauthorized);
                }
                let metadata = core.authority.control_grant(peer, policy, &grant, now)?;
                caller(request, &metadata.request)?;
                let challenge =
                    core.authority
                        .refresh_challenge(peer, policy, &slot.session, &grant, now)?;
                signed_challenge(
                    self.0.host,
                    &mut sign,
                    request,
                    peer,
                    policy,
                    core,
                    metadata.request,
                    challenge,
                    now,
                )
            }
            ControlCommand::RefreshComplete {
                session_id,
                nonce,
                signature,
            } => {
                caller(request, core.authority.pending_control_request(nonce)?)?;
                let slot = core
                    .sessions
                    .get_mut(&session_id)
                    .ok_or(MediaError::StaleGeneration)?;
                caller(request, &slot.session.metadata().request)?;
                if slot.session.metadata().root != binding.device_root {
                    return Err(MediaError::Unauthorized);
                }
                let alive = slot.alive.upgrade().ok_or(MediaError::StaleGeneration)?;
                let session = core.authority.refresh_relay(
                    peer,
                    policy,
                    &slot.session,
                    nonce,
                    tilo_relay::delivery_v3::media_wire::join_signature(signature),
                    now,
                )?;
                let expires_at_ns = session.metadata().expires_at_ns;
                alive
                    .deadline_ns
                    .store(self.0.deadline(expires_at_ns, now)?, Ordering::Release);
                slot.session = session;
                Ok(ControlResult::Refreshed {
                    session_id,
                    expires_at_ns,
                })
            }
            ControlCommand::RelayClose { session_id } => {
                let slot = core
                    .sessions
                    .get(&session_id)
                    .ok_or(MediaError::StaleGeneration)?;
                caller(request, &slot.session.metadata().request)?;
                if slot.session.metadata().root != binding.device_root {
                    return Err(MediaError::Unauthorized);
                }
                core.authority.close_relay(&slot.session)?;
                core.sessions.remove(&session_id);
                Ok(ControlResult::Closed)
            }
            ControlCommand::Presence { session_id } => {
                let slot = core
                    .sessions
                    .get(&session_id)
                    .ok_or(MediaError::StaleGeneration)?;
                caller(request, &slot.session.metadata().request)?;
                if slot.session.metadata().root != binding.device_root {
                    return Err(MediaError::Unauthorized);
                }
                core.authority
                    .presence_snapshot(peer, policy, &slot.session, now)?;
                let intent = slot.session.metadata().request.clone();
                let expiry =
                    core.authority
                        .control_presence_expiry(peer, policy, &slot.session, now)?;
                signed_presence(
                    self.0.host,
                    &mut sign,
                    request,
                    peer,
                    policy,
                    core,
                    intent,
                    now,
                    expiry,
                )
            }
        })
    }
}
fn context(
    host: [u8; 32],
    http: &VerifiedRequest<'_>,
    peer: &StatePeer,
    policy: &dyn MediaCallPolicy,
    core: &mut Core,
    intent: GrantRequest,
    now: SystemTime,
    issued: u64,
    expires: u64,
) -> Result<MediaContext, MediaError> {
    let (current, boot, own, names) = core.authority.control_view(peer, policy, &intent, now)?;
    if !core.inventories.contains_key(&own.call)
        && core.inventories.len() >= MAX_MEDIA_PARTICIPATION_KEYS
    {
        return Err(MediaError::Capacity);
    }
    let revision = if intent.directions.subscribe {
        let entry = core
            .inventories
            .entry(own.call)
            .or_insert((1, names.clone()));
        if entry.1 != names {
            entry.0 = entry.0.checked_add(1).ok_or(MediaError::Capacity)?;
            entry.1 = names.clone();
        }
        entry.0
    } else {
        1
    };
    let binding = http.session().binding();
    Ok(MediaContext {
        version: 1,
        descriptor_digest: binding.descriptor_digest,
        delivery_session_id: http.session().session_id(),
        delivery_boot: binding.server_boot_id,
        origin: binding.origin.clone(),
        workspace: binding.workspace,
        room: peer.room().into(),
        root: binding.device_root,
        delivery_host: binding.host,
        media_host: host,
        media_boot: boot,
        request: intent,
        epoch: current.epoch,
        membership_generation: current.membership_generation,
        directions: current.directions,
        inventory: PublisherInventory {
            revision,
            namespaces: names.into_iter().filter(|v| *v != own).collect(),
        },
        issued_at_ns: issued,
        expires_at_ns: expires,
    })
}
fn signed_challenge(
    host: [u8; 32],
    sign: &mut impl FnMut(MediaSigningInput) -> Result<[u8; 64], MediaError>,
    http: &VerifiedRequest<'_>,
    peer: &StatePeer,
    policy: &dyn MediaCallPolicy,
    core: &mut Core,
    intent: GrantRequest,
    challenge: MediaChallenge,
    now: SystemTime,
) -> Result<ControlResult, MediaError> {
    let (issued, expires) = match &challenge.transcript {
        ChallengeTranscript::Native(v) => (v.issued_at_ns, v.expires_at_ns),
        ChallengeTranscript::Relay(v) => (v.issued_at_ns, v.expires_at_ns),
        ChallengeTranscript::Refresh(v) => (v.issued_at_ns, v.expires_at_ns),
    };
    let mut signed = SignedChallenge {
        context: context(host, http, peer, policy, core, intent, now, issued, expires)?,
        transcript: challenge.transcript,
        signature: [[0; 32]; 2],
    };
    signed.signature = split_signature(sign_checked(
        host,
        sign,
        signed
            .signing_digest()
            .map_err(|_| MediaError::InvalidProof)?,
    )?);
    Ok(ControlResult::Challenge(signed))
}
fn signed_presence(
    host: [u8; 32],
    sign: &mut impl FnMut(MediaSigningInput) -> Result<[u8; 64], MediaError>,
    http: &VerifiedRequest<'_>,
    peer: &StatePeer,
    policy: &dyn MediaCallPolicy,
    core: &mut Core,
    intent: GrantRequest,
    now: SystemTime,
    expires: u64,
) -> Result<ControlResult, MediaError> {
    let issued: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| MediaError::Expired)?
        .as_nanos()
        .try_into()
        .map_err(|_| MediaError::Expired)?;
    let expiry = issued
        .checked_add(MEDIA_PRESENCE_LEASE_NS)
        .ok_or(MediaError::Expired)?
        .min(expires);
    let mut signed = SignedPresence {
        context: context(host, http, peer, policy, core, intent, now, issued, expiry)?,
        signature: [[0; 32]; 2],
    };
    signed.signature = split_signature(sign_checked(
        host,
        sign,
        signed
            .signing_digest()
            .map_err(|_| MediaError::InvalidProof)?,
    )?);
    Ok(ControlResult::Present(signed))
}

fn sign_checked(
    host: [u8; 32],
    sign: &mut impl FnMut(MediaSigningInput) -> Result<[u8; 64], MediaError>,
    digest: [u8; 32],
) -> Result<[u8; 64], MediaError> {
    let signature = sign(MediaSigningInput::new(digest))?;
    ed25519_dalek::VerifyingKey::from_bytes(&host)
        .map_err(|_| MediaError::Unauthorized)?
        .verify_strict(&digest, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| MediaError::Unauthorized)?;
    Ok(signature)
}
