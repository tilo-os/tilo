//! Carrier registries across descriptor renewals. A renewal replaces only the
//! issuing registry: sessions authenticated under a superseded descriptor keep
//! their original binding until they expire or are replaced, instead of every
//! client being forced through a fresh handshake every renewal period.
//!
//! A retained session is admitted only with a context freshly derived from
//! the *current* verified descriptor under the current Store turn; the retained
//! identity (boot, digest, generation) is substituted only after checking that
//! context is exactly the current registry's. Descriptor renewal never changes
//! origin, capabilities, media mask or host (the service renews with its fixed
//! installed configuration), so the substituted binding differs only in the
//! descriptor issuance identity. Device, host, roster and revocation checks
//! remain the current turn's.
use anyhow::{Context as _, Result, ensure};
use qos_relay::delivery_v3::{
    auth::{self, Binding},
    http_proof::{Request, RequestProof, VerifiedRequest},
};
use qos_session_peer::ingress::{
    BufferedRequest, CarrierFinish, CarrierIntent, CarrierReady, CarrierRegistry, CarrierTicket,
    PendingCarrier,
};
use std::collections::BTreeMap;

/// Superseded registries kept for their live sessions. Sessions last at most
/// 15 minutes and renewal runs every ~12, so two cover every live session.
const MAX_RETAINED: usize = 2;

#[derive(Clone, Copy)]
struct Tracked {
    device: [u8; 32],
    capabilities: u64,
    expires: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    boot: [u8; 16],
    descriptor: [u8; 32],
    generation: u64,
}
struct Generation {
    registry: CarrierRegistry,
    identity: Identity,
    sessions: BTreeMap<[u8; 16], Tracked>,
}
enum Slot {
    Current,
    Retained(usize),
}

pub(crate) struct Registries {
    current: Generation,
    retained: Vec<Generation>,
}

fn context_with<'c, R>(
    context: &auth::VerificationContext<'c>,
    binding: &Binding,
    action: impl FnOnce(&auth::VerificationContext<'_>) -> R,
) -> R {
    let retargeted = auth::VerificationContext {
        binding,
        capabilities: context.capabilities,
        credentials: context.credentials,
        authority: context.authority,
        now_ns: context.now_ns,
    };
    action(&retargeted)
}

impl Registries {
    pub(crate) fn new(registry: CarrierRegistry, descriptor: [u8; 32], generation: u64) -> Self {
        let identity = Identity {
            boot: registry.boot_id(),
            descriptor,
            generation,
        };
        Self {
            current: Generation {
                registry,
                identity,
                sessions: BTreeMap::new(),
            },
            retained: Vec::new(),
        }
    }
    /// Install the registry for a renewed descriptor. Returns session IDs of a
    /// superseded registry dropped beyond the retention bound; the caller must
    /// fence their held work.
    pub(crate) fn rotate(
        &mut self,
        registry: CarrierRegistry,
        descriptor: [u8; 32],
        generation: u64,
    ) -> Result<Vec<[u8; 16]>> {
        ensure!(
            generation > self.current.identity.generation,
            "descriptor generation did not advance"
        );
        let identity = Identity {
            boot: registry.boot_id(),
            descriptor,
            generation,
        };
        let previous = std::mem::replace(
            &mut self.current,
            Generation {
                registry,
                identity,
                sessions: BTreeMap::new(),
            },
        );
        let mut fenced = Vec::new();
        if !previous.sessions.is_empty() {
            self.retained.insert(0, previous);
        }
        while self.retained.len() > MAX_RETAINED {
            let dropped = self.retained.pop().expect("nonempty");
            fenced.extend(dropped.sessions.keys().copied());
        }
        Ok(fenced)
    }
    /// Release superseded registries whose sessions all expired or were replaced.
    pub(crate) fn prune(&mut self, now: u64) {
        for generation in std::iter::once(&mut self.current).chain(self.retained.iter_mut()) {
            generation
                .sessions
                .retain(|_, tracked| tracked.expires > now);
        }
        self.retained
            .retain(|generation| !generation.sessions.is_empty());
    }
    fn slot(&self, id: [u8; 16]) -> Option<Slot> {
        if self.current.registry.session_intent(id).is_ok() {
            return Some(Slot::Current);
        }
        self.retained
            .iter()
            .position(|generation| generation.registry.session_intent(id).is_ok())
            .map(Slot::Retained)
    }
    /// The binding a retained generation's sessions were authenticated with,
    /// derived from a fresh current context.
    fn retarget(&self, index: usize, current: &Binding) -> Result<Binding> {
        let own = self.current.identity;
        let retained = self
            .retained
            .get(index)
            .context("retained registry missing")?
            .identity;
        ensure!(
            current.server_boot_id == own.boot
                && current.descriptor_digest == own.descriptor
                && current.descriptor_generation == own.generation
                && retained.generation < own.generation,
            "context is not the current descriptor"
        );
        let mut binding = current.clone();
        binding.server_boot_id = retained.boot;
        binding.descriptor_digest = retained.descriptor;
        binding.descriptor_generation = retained.generation;
        Ok(binding)
    }
    /// The exact binding a live session must be verified against.
    pub(crate) fn binding_for(&self, id: [u8; 16], current: &Binding) -> Result<Binding> {
        match self.slot(id).context("unknown session")? {
            Slot::Current => Ok(current.clone()),
            Slot::Retained(index) => self.retarget(index, current),
        }
    }
    pub(crate) fn observe_time(&mut self, now: u64) -> Result<()> {
        self.current.registry.observe_time(now)
    }

    // New sessions are only ever issued by the current registry.
    pub(crate) fn challenge_in(
        &mut self,
        body: &[u8],
        context: &auth::VerificationContext<'_>,
        key: &ed25519_dalek::SigningKey,
    ) -> Result<Vec<u8>> {
        self.current.registry.challenge_in(body, context, key)
    }
    pub(crate) fn finish_intent(&self, body: &[u8]) -> Result<CarrierIntent> {
        self.current.registry.finish_intent(body)
    }
    pub(crate) fn finish_in(
        &mut self,
        body: &[u8],
        context: &auth::VerificationContext<'_>,
    ) -> Result<CarrierFinish> {
        self.current.registry.finish_in(body, context)
    }
    /// Publish in the current registry; a new data session also replaces the
    /// same device's data sessions retained from earlier descriptors.
    pub(crate) fn publish_in(
        &mut self,
        finish: CarrierFinish,
        context: &auth::VerificationContext<'_>,
    ) -> Result<CarrierReady> {
        let session = finish.session();
        let id = session.session_id();
        let tracked = Tracked {
            device: session.device_certificate().device_public_key,
            capabilities: session.capabilities(),
            expires: session.expires_at_ns(),
        };
        let mut ready = self.current.registry.publish_in(finish, context)?;
        for retired in &ready.retired {
            self.current.sessions.remove(retired);
        }
        self.current.sessions.insert(id, tracked);
        if tracked.capabilities != 0 {
            for generation in &mut self.retained {
                let replaced = generation
                    .sessions
                    .iter()
                    .filter(|(_, other)| other.device == tracked.device && other.capabilities != 0)
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                for id in replaced {
                    generation.registry.retire_session(id);
                    generation.sessions.remove(&id);
                    ready.retired.push(id);
                }
            }
            self.retained
                .retain(|generation| !generation.sessions.is_empty());
        }
        Ok(ready)
    }
    pub(crate) fn retire_session(&mut self, id: [u8; 16]) -> bool {
        let mut retired = false;
        for generation in std::iter::once(&mut self.current).chain(self.retained.iter_mut()) {
            generation.sessions.remove(&id);
            retired |= generation.registry.retire_session(id);
        }
        self.retained
            .retain(|generation| !generation.sessions.is_empty());
        retired
    }
    pub(crate) fn session_intent(&self, id: [u8; 16]) -> Result<CarrierIntent> {
        match self.slot(id).context("unknown session")? {
            Slot::Current => self.current.registry.session_intent(id),
            Slot::Retained(index) => self.retained[index].registry.session_intent(id),
        }
    }
    pub(crate) fn admit_in<'a>(
        &'a mut self,
        proof: &RequestProof,
        request: &'a Request<'a>,
        context: &auth::VerificationContext<'_>,
    ) -> Result<VerifiedRequest<'a>> {
        match self.slot(proof.session_id()).context("unknown session")? {
            Slot::Current => self.current.registry.admit_in(proof, request, context),
            Slot::Retained(index) => {
                let binding = self.retarget(index, context.binding)?;
                let registry = &mut self.retained[index].registry;
                context_with(context, &binding, |context| {
                    registry.admit_in(proof, request, context)
                })
            }
        }
    }
    pub(crate) fn revalidate_in(
        &mut self,
        id: [u8; 16],
        context: &auth::VerificationContext<'_>,
    ) -> Result<&auth::VerifiedSession> {
        match self.slot(id).context("unknown session")? {
            Slot::Current => self.current.registry.revalidate_in(id, context),
            Slot::Retained(index) => {
                let binding = self.retarget(index, context.binding)?;
                let registry = &mut self.retained[index].registry;
                context_with(context, &binding, |context| {
                    registry.revalidate_in(id, context)
                })
            }
        }
    }
    pub(crate) fn admit_buffered_in<'a>(
        &'a mut self,
        proof: &RequestProof,
        exact: &'a Request<'a>,
        original: &BufferedRequest,
        context: &auth::VerificationContext<'_>,
    ) -> Result<(VerifiedRequest<'a>, CarrierTicket)> {
        match self.slot(proof.session_id()).context("unknown session")? {
            Slot::Current => self
                .current
                .registry
                .admit_buffered_in(proof, exact, original, context),
            Slot::Retained(index) => {
                let binding = self.retarget(index, context.binding)?;
                let registry = &mut self.retained[index].registry;
                context_with(context, &binding, |context| {
                    registry.admit_buffered_in(proof, exact, original, context)
                })
            }
        }
    }
    pub(crate) fn resume_buffered_in<'a>(
        &'a mut self,
        pending: &'a PendingCarrier,
        context: &auth::VerificationContext<'_>,
    ) -> Result<&'a auth::VerifiedSession> {
        match self
            .slot(pending.session_id())
            .context("operation session replaced")?
        {
            Slot::Current => self.current.registry.resume_buffered_in(pending, context),
            Slot::Retained(index) => {
                let binding = self.retarget(index, context.binding)?;
                let registry = &mut self.retained[index].registry;
                context_with(context, &binding, |context| {
                    registry.resume_buffered_in(pending, context)
                })
            }
        }
    }
}
