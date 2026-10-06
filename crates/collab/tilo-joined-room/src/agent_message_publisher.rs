//! Host-only publisher registration and publication/retirement linearization.
//! Neither these registrations nor their leases travel to the SDK. The native
//! installed broker owns exact worker/directory/stream identities; the pinned
//! host signs their registration and the owner alone constructs the lease.
use crate::{NativeWorkspaceOwner, RoomId, SharedOwner};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, Ordering},
};
use std::time::SystemTime;
use tilo_user_identity::DeviceCert;

const DOMAIN: &[u8] = b"tilo/native-agent-message-publisher/v1\0";
const RESERVED: u8 = 1;
const RETIRED: u8 = 2;
const MAX_PUBLISHERS: usize = 256;

/// Inert exact installation identities, not a caller assertion of authority.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessagePublisherData {
    pub room: RoomId,
    pub worker: uuid::Uuid,
    pub directory: uuid::Uuid,
    pub stream: uuid::Uuid,
    pub installation: uuid::Uuid,
}

/// Signed only by the private installed native host service. It is not sent to
/// an application, and signature possession is not a publication reservation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessagePublisherRegistration {
    pub data: AgentMessagePublisherData,
    pub attestor: DeviceCert,
    pub signature: Vec<u8>,
}

impl AgentMessagePublisherRegistration {
    /// Compute signing input only; this cannot mint a live lease.
    ///
    /// # Errors
    /// A required installation identity is absent or encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        if [
            self.data.worker,
            self.data.directory,
            self.data.stream,
            self.data.installation,
        ]
        .iter()
        .any(uuid::Uuid::is_nil)
        {
            return Err("Agent Message publisher has an absent installation identity".into());
        }
        let bytes = postcard::to_allocvec(&(&self.data, &self.attestor))
            .map_err(|_| "Agent Message publisher registration encoding failed")?;
        let mut hash = blake3::Hasher::new();
        hash.update(DOMAIN);
        hash.update(&bytes);
        Ok(hash.finalize().as_bytes().to_vec())
    }
}

pub(crate) struct PublisherState {
    owner: Weak<SharedOwner>,
    room: RoomId,
    digest: [u8; 32],
    state: AtomicU8,
}

#[derive(Default)]
pub(crate) struct PublisherRegistry {
    live: BTreeMap<[u8; 32], Arc<PublisherState>>,
    // Retained owner-lifetime replay tombstones, not an evictable bounded cache.
    retired: BTreeSet<[u8; 32]>,
}

impl PublisherRegistry {
    fn collect_retired(&mut self) {
        self.live.retain(|digest, state| {
            if state.state.load(Ordering::Acquire) & RETIRED != 0 && Arc::strong_count(state) == 1 {
                self.retired.insert(*digest);
                false
            } else {
                true
            }
        });
    }
}

/// Owner-issued, nonserializable exact native installation lease. It contains
/// no signing key, process authority, arbitrary verifier or public constructor.
#[derive(Clone)]
pub struct AgentMessagePublisherLease(Arc<PublisherState>);

impl AgentMessagePublisherLease {
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        self.0.digest
    }

    /// Retire on actual native directory/stream replacement or disconnect.
    /// Already reserved publication is ordered before retirement; no later
    /// publication may reserve this lease. Retirement can never be reversed.
    pub fn retire(&self) {
        self.0.state.fetch_or(RETIRED, Ordering::AcqRel);
    }

    pub(crate) fn check_identity(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        digest: &[u8; 32],
    ) -> Result<(), String> {
        if !Weak::ptr_eq(&self.0.owner, &Arc::downgrade(&owner.0))
            || self.0.room != room
            || self.0.digest != *digest
        {
            return Err("Agent Message publisher belongs to another owner or installation".into());
        }
        Ok(())
    }

    pub(crate) fn reserve(&self) -> Result<PublisherReservation, String> {
        #[cfg(test)]
        publication_test_gate(false);
        // This CAS is publication admission's linearization point. Persisting
        // may finish later: retirement after this point cannot retroactively
        // turn already admitted publication into an unperformed effect.
        self.0
            .state
            .compare_exchange(0, RESERVED, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Agent Message publisher retired before publication admission")?;
        let reservation = PublisherReservation(self.clone());
        #[cfg(test)]
        publication_test_gate(true);
        Ok(reservation)
    }
}

#[cfg(test)]
type PublicationTestGate = Box<dyn Fn(bool)>;

#[cfg(test)]
thread_local! {
    static PUBLICATION_TEST_GATE: std::cell::RefCell<Option<PublicationTestGate>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn publication_test_gate(reserved: bool) {
    PUBLICATION_TEST_GATE.with_borrow(|gate| {
        if let Some(gate) = gate {
            gate(reserved);
        }
    });
}

#[cfg(test)]
#[path = "agent_message_publisher_tests.rs"]
mod tests;

pub(crate) struct PublisherReservation(AgentMessagePublisherLease);

impl Drop for PublisherReservation {
    fn drop(&mut self) {
        // Both definite refusal and uncertain persistence release ownership;
        // neither may clear retirement. Uncertain owner persistence separately
        // fences the owner, so release never asserts that an effect was absent.
        self.0.0.state.fetch_and(!RESERVED, Ordering::AcqRel);
    }
}

impl NativeWorkspaceOwner {
    /// Install exact host-private publisher custody, never through a guest RPC.
    ///
    /// # Errors
    /// Signature/current pinned host mismatch, retired identity replay, or the
    /// bounded live installation registry is full.
    pub fn register_agent_message_publisher(
        &self,
        registration: &AgentMessagePublisherRegistration,
        now: SystemTime,
    ) -> Result<AgentMessagePublisherLease, String> {
        let bytes = registration.signing_bytes()?;
        let digest: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| "Agent Message publisher registration digest is invalid")?;
        let signature = ed25519_dalek::Signature::from_slice(&registration.signature)
            .map_err(|_| "Agent Message publisher registration signature is invalid")?;
        ed25519_dalek::VerifyingKey::from_bytes(&registration.attestor.device_public_key)
            .map_err(|_| "Agent Message publisher registration key is invalid")?
            .verify_strict(&bytes, &signature)
            .map_err(|_| "Agent Message publisher registration signature mismatch")?;
        let _turn = self.0.mutation.lock();
        self.with_current(registration.data.room, |view| {
            let pinned = view.host().unwrap_or_else(|| view.governor());
            if &registration.attestor != pinned
                || &registration.attestor != view.device()
                || !view.peer().certificates().contains(&registration.attestor)
                || (view.host().is_some() && !registration.attestor.capabilities.can_act_as_host)
            {
                return Err("Agent Message publisher lacks current pinned host custody".into());
            }
            view.peer()
                .permissions(registration.attestor.device_public_key, now)?;
            Ok(())
        })?;
        let mut publishers = self.0.message_publishers.lock();
        publishers.collect_retired();
        if let Some(existing) = publishers.live.get(&digest) {
            if existing.state.load(Ordering::Acquire) & RETIRED != 0 {
                return Err("Agent Message publisher registration was retired".into());
            }
            return Ok(AgentMessagePublisherLease(existing.clone()));
        }
        if publishers.retired.contains(&digest) {
            return Err("Agent Message publisher registration was retired".into());
        }
        if publishers.live.len() >= MAX_PUBLISHERS {
            return Err("Agent Message publisher installation registry is full".into());
        }
        let state = Arc::new(PublisherState {
            owner: Arc::downgrade(&self.0),
            room: registration.data.room,
            digest,
            state: AtomicU8::new(0),
        });
        publishers.live.insert(digest, state.clone());
        drop(publishers);
        Ok(AgentMessagePublisherLease(state))
    }
}
