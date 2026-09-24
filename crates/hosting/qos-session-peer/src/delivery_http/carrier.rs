//! Owner-neutral authentication machinery. This is never installed readiness.
//! The concrete owner supplies a fresh borrowed context under its actual lock,
//! performs its consumer barrier, and only then publishes the finished session.
use super::*;
use tilo_relay::delivery_v3::descriptor::VerifiedDescriptor;

/// Inert lookup fields used only to construct a fresh concrete owner context.
#[derive(Clone, Copy)]
pub struct CarrierIntent(pub(super) Identity);
impl CarrierIntent {
    pub fn device(&self) -> [u8; 32] {
        self.0.device
    }
    pub fn requested(&self) -> u64 {
        self.0.requested
    }
    pub fn mandatory(&self) -> u64 {
        self.0.mandatory
    }
}
/// Successful mutual authentication, not a durable consumer or installed route.
pub struct CarrierFinish {
    pub(super) verified: auth::VerifiedSession,
    pub(super) identity: Identity,
}
impl CarrierFinish {
    pub fn session(&self) -> &auth::VerifiedSession {
        &self.verified
    }
}
/// Inert response and retired-session IDs, only after final fresh verification.
pub struct CarrierReady {
    pub bytes: Vec<u8>,
    pub retired: Vec<[u8; 16]>,
}
impl HttpSessions {
    /// Resource/transport mechanics only. This cannot qualify a concrete owner.
    pub fn new_unqualified(descriptor: &VerifiedDescriptor, policy: u64, now: u64) -> Result<Self> {
        ensure!(
            tilo_relay::delivery_v3::capabilities::validate(policy),
            "unknown data capability"
        );
        let mut boot = [0; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut boot)?;
        ensure!(boot != [0; 16], "invalid entropy");
        Ok(Self {
            native_time: None,
            // Transcript extensions (multi-group replay, chained polls) are
            // negotiated beside the descriptor bits and never advertised in
            // the descriptor. This host serves PIPELINED_POLL_V1.
            capabilities: tilo_relay::delivery_v3::capabilities::pipelined_policy(
                policy & descriptor.delivery_capabilities(),
            ),
            boot,
            descriptor: descriptor.descriptor_digest(),
            descriptor_generation: descriptor.generation(),
            continuity: 0,
            high_water: now,
            next_entry: 0,
            pending: BTreeMap::new(),
            live: BTreeMap::new(),
            context_cache: Default::default(),
        })
    }
    /// Withdraw only this registry entry; durable consumer/stage retirement is
    /// the concrete owner's decision. This operation cannot grant authority.
    pub fn retire_session(&mut self, id: [u8; 16]) -> bool {
        self.live.remove(&id).is_some()
    }
    pub fn boot_id(&self) -> [u8; 16] {
        self.boot
    }
    pub fn challenge_intent(body: &[u8]) -> Result<CarrierIntent> {
        Ok(CarrierIntent(decode_challenge(body)?.0))
    }
    pub fn finish_intent(&self, body: &[u8]) -> Result<CarrierIntent> {
        let proof = auth::ClientProof::decode(body)?;
        Ok(CarrierIntent(
            self.pending
                .get(&proof.session_id)
                .context("unknown or expired challenge")?
                .identity,
        ))
    }
    pub fn session_intent(&self, id: [u8; 16]) -> Result<CarrierIntent> {
        Ok(CarrierIntent(
            self.live.get(&id).context("unknown session")?.identity,
        ))
    }
    fn check_context(
        &self,
        identity: Identity,
        context: &auth::VerificationContext<'_>,
    ) -> Result<()> {
        let binding = context.binding;
        let (boot, descriptor) = identity
            .superseded
            .map_or((self.boot, self.descriptor), |pin| {
                (pin.boot, pin.descriptor)
            });
        ensure!(
            binding.server_boot_id == boot
                && binding.descriptor_digest == descriptor
                && binding.carrier == auth::Carrier::Https
                && binding.device == identity.device
                && binding.requested_capabilities == identity.requested
                && binding.mandatory_capabilities == identity.mandatory
                && context.capabilities.supported & !self.capabilities == 0
                && context.capabilities.allowed & !self.capabilities == 0,
            "carrier context changed"
        );
        Ok(())
    }
    pub fn challenge_in(
        &mut self,
        body: &[u8],
        context: &auth::VerificationContext<'_>,
        key: &ed25519_dalek::SigningKey,
    ) -> Result<Vec<u8>> {
        self.advance(context.now_ns)?;
        let (identity, nonce) = decode_challenge(body)?;
        self.check_context(identity, context)?;
        ensure!(
            self.pending.len() < PENDING_LIMIT
                && self
                    .pending
                    .values()
                    .filter(|v| v.identity.device == identity.device)
                    .count()
                    < PENDING_DEVICE,
            "pending capacity"
        );
        ensure!(self.has_capacity(identity), "live capacity");
        let (challenge, proof) = auth::Challenge::issue(context, nonce, key)?;
        let expires = context
            .now_ns
            .checked_add(PENDING_NS)
            .context("challenge expiry overflow")?
            .min(proof.transcript.expires_at_ns);
        let bytes = proof.encode()?;
        ensure!(
            !self.pending.contains_key(&proof.transcript.session_id)
                && !self.live.contains_key(&proof.transcript.session_id),
            "session entropy collision"
        );
        self.pending.insert(
            proof.transcript.session_id,
            Pending {
                identity,
                challenge,
                expires,
            },
        );
        Ok(bytes)
    }
    pub fn finish_in(
        &mut self,
        body: &[u8],
        context: &auth::VerificationContext<'_>,
    ) -> Result<CarrierFinish> {
        self.advance(context.now_ns)?;
        let proof = auth::ClientProof::decode(body)?;
        let pending = self
            .pending
            .remove(&proof.session_id)
            .context("unknown or expired challenge")?;
        self.check_context(pending.identity, context)?;
        ensure!(self.has_capacity(pending.identity), "live capacity");
        let verified = pending.challenge.finish(&proof, context)?;
        Ok(CarrierFinish {
            verified,
            identity: pending.identity,
        })
    }
    /// The concrete owner must complete its consumer commit before calling this.
    /// This unqualified registry cannot attest to that owner-specific barrier.
    pub fn publish_in(
        &mut self,
        finish: CarrierFinish,
        context: &auth::VerificationContext<'_>,
    ) -> Result<CarrierReady> {
        self.advance(context.now_ns)?;
        let CarrierFinish { verified, identity } = finish;
        self.check_context(identity, context)?;
        let id = verified.session_id();
        let expires = verified.expires_at_ns();
        let capabilities = verified.capabilities();
        let digest =
            http_proof::session_binding_digest(verified.binding(), id, capabilities, expires)?;
        let admission = HttpSessionAdmission::new(verified, context)?;
        let bytes = postcard::to_stdvec(&(3_u16, id, digest, expires, capabilities))?;
        ensure!(!self.live.contains_key(&id), "session collision");
        let entry = self
            .next_entry
            .checked_add(1)
            .context("session entry serial exhausted")?;
        let retired = self
            .live
            .iter()
            .filter_map(|(id, live)| {
                (capabilities != 0
                    && live.capabilities != 0
                    && live.identity.device == identity.device)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        // Capacity and mutation must use this same actual negotiated retirement
        // set. Optional requested data bits can legitimately negotiate to zero.
        let retained = self
            .live
            .iter()
            .filter(|(id, live)| live.identity.superseded.is_none() && !retired.contains(id));
        ensure!(
            retained.clone().count() < LIVE_LIMIT
                && retained
                    .filter(|(_, live)| live.identity.device == identity.device)
                    .count()
                    < LIVE_DEVICE,
            "live capacity"
        );
        for id in &retired {
            self.live.remove(id);
        }
        self.next_entry = entry;
        self.live.insert(
            id,
            Live {
                private_slots: (capabilities
                    & (tilo_relay::delivery_v3::capabilities::PRIVATE_COMMANDS_V1
                        | tilo_relay::delivery_v3::capabilities::PRIVATE_READS_V1)
                    != 0)
                    .then(|| std::sync::Arc::new(tokio::sync::Semaphore::new(8))),
                entry,
                capabilities,
                identity,
                admission,
                expires,
            },
        );
        Ok(CarrierReady { bytes, retired })
    }
    pub fn admit_in<'a>(
        &'a mut self,
        proof: &RequestProof,
        request: &'a Request<'a>,
        context: &auth::VerificationContext<'_>,
    ) -> Result<VerifiedRequest<'a>> {
        self.advance(context.now_ns)?;
        let id = proof.session_id();
        let identity = self.live.get(&id).context("unknown session")?.identity;
        self.check_context(identity, context)?;
        Ok(self
            .live
            .get_mut(&id)
            .context("unknown session")?
            .admission
            .admit(proof, request, context)?)
    }
    /// Session freshness only. Continuations additionally need original request,
    /// cancellation/deadline and entry custody; this is not request admission.
    pub fn revalidate_in(
        &mut self,
        id: [u8; 16],
        context: &auth::VerificationContext<'_>,
    ) -> Result<&auth::VerifiedSession> {
        self.advance(context.now_ns)?;
        let identity = self.live.get(&id).context("unknown session")?.identity;
        self.check_context(identity, context)?;
        Ok(self
            .live
            .get_mut(&id)
            .context("unknown session")?
            .admission
            .revalidate(context)?)
    }
}
fn decode_challenge(body: &[u8]) -> Result<(Identity, [u8; 32])> {
    ensure!(body.len() <= 128, "challenge exceeds bound");
    let wire: (u16, [u8; 32], [u8; 32], u64, u64) = postcard::from_bytes(body)?;
    ensure!(
        wire.0 == 3
            && wire.1 != [0; 32]
            && wire.2 != [0; 32]
            && postcard::to_stdvec(&wire)? == body,
        "invalid canonical challenge"
    );
    Ok((
        Identity {
            device: wire.1,
            requested: wire.3,
            mandatory: wire.4,
            superseded: None,
        },
        wire.2,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery_http::tests::Fixture;
    #[test]
    fn carrier_uses_actual_borrowed_context_and_consumes_nonce_once() {
        let f = Fixture::new();
        let guard = f.current.read().unwrap();
        let descriptor = f
            .owner
            .verified_delivery_transport(
                guard.view(),
                at(f.now),
                crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            )
            .unwrap();
        let mut registry = HttpSessions::new_unqualified(&descriptor, 0, f.now).unwrap();
        // Native context adapter is used only as this test's genuine owner source.
        registry.continuity = guard.continuity();
        registry.native_time = Some(guard.view().policy_time().unwrap());
        let body = f.request([219; 32]);
        let intent = HttpSessions::challenge_intent(&body).unwrap();
        assert_eq!(
            intent.device(),
            f.owner.identity.native_cert.device_public_key
        );
        let context = registry
            .context(&f.owner, guard.view(), intent.0, f.now)
            .unwrap();
        let wire = registry
            .challenge_in(&body, &context.verification(), &f.owner.identity.host)
            .unwrap();
        drop(guard);
        let proof = auth::ServerProof::decode(&wire).unwrap();
        let client = f.client(&proof, [219; 32]).encode().unwrap();
        let intent = registry.finish_intent(&client).unwrap();
        let guard = f.current.read().unwrap();
        let context = registry
            .context(&f.owner, guard.view(), intent.0, f.now)
            .unwrap();
        let finish = registry
            .finish_in(&client, &context.verification())
            .unwrap();
        let id = finish.session().session_id();
        assert!(
            registry.session_intent(id).is_err(),
            "finish alone cannot publish a live entry"
        );
        let ready = registry
            .publish_in(finish, &context.verification())
            .unwrap();
        assert!(ready.retired.is_empty());
        let ready: (u16, [u8; 16], [u8; 32], u64, u64) =
            postcard::from_bytes(&ready.bytes).unwrap();
        let request = Request::new("GET", "/peer/v3/events?mode=poll", &[]).unwrap();
        let request_proof = RequestProof::sign(
            id,
            ready.2,
            &request,
            f.now,
            f.now + 1_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        registry
            .admit_in(&request_proof, &request, &context.verification())
            .unwrap();
        registry.revalidate_in(id, &context.verification()).unwrap();
        assert!(
            registry
                .admit_in(&request_proof, &request, &context.verification())
                .is_err()
        );
        let mut wrong_binding = context.binding.clone();
        wrong_binding.server_boot_id = [7; 16];
        let mut wrong = context.verification();
        wrong.binding = &wrong_binding;
        assert!(registry.revalidate_in(id, &wrong).is_err());
        wrong.now_ns = f.now + 10;
        assert!(registry.revalidate_in(id, &wrong).is_err());
        assert!(
            registry.revalidate_in(id, &context.verification()).is_err(),
            "denied context still advances clock floor"
        );
    }
    #[test]
    fn carrier_buffered_continuation_owns_actual_lifecycle_and_never_readmits_nonce() {
        let f = Fixture::new();
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let (id, digest) = f.handshake(&mut registry, [228; 32]);
        let exact = Request::new("GET", "/peer/v3/events?mode=poll", &[]).unwrap();
        let proof = RequestProof::sign(
            id,
            digest,
            &exact,
            f.now,
            f.now + 1_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let (ingress, _receiver) = crate::https::ingress_channel();
        let (original, client) = crate::https::tests::data_test_request(
            &ingress,
            "GET",
            exact.target(),
            &[],
            &proof.encode().unwrap(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        let guard = f.current.read().unwrap();
        let identity = registry.session_intent(id).unwrap().0;
        let context = registry
            .context(&f.owner, guard.view(), identity, f.now)
            .unwrap();
        let (verified, ticket) = registry
            .admit_buffered_in(&proof, &exact, &original, &context.verification())
            .unwrap();
        assert_eq!(verified.session().session_id(), id);
        drop(verified);
        let pending = ticket.park(original).unwrap();
        registry
            .resume_buffered_in(&pending, &context.verification())
            .unwrap();
        assert!(
            registry
                .admit_in(&proof, &exact, &context.verification())
                .is_err()
        );
        registry.live.get_mut(&id).unwrap().entry += 1;
        assert!(
            registry
                .resume_buffered_in(&pending, &context.verification())
                .is_err()
        );
        registry.live.get_mut(&id).unwrap().entry -= 1;
        client.cancel();
        assert!(
            registry
                .resume_buffered_in(&pending, &context.verification())
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "carrier/capacity_tests.rs"]
mod capacity_tests;
