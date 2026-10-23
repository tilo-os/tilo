//! Bounded bridge from authenticated record sessions to later blob requests.
//!
//! Blob transport connections can arrive after the record-sharing handshake.
//! This registry remembers *which certified person/device opened which exact
//! record session*, but never caches an authorization decision: every lookup
//! reruns [`RecordAuthorizer`] against the live [`ShareSession`].  Callers get
//! record IDs, not hashes, and must derive reachable manifests/chunks from the
//! current authorized record graph.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use collab_types::Ed25519PublicKey;
use fs_protocol::FileRecordId;
use thiserror::Error;
use tilo_clock::{Clock, MonotonicClock};
use uuid::Uuid;

use crate::peer_auth::AuthenticatedFsPeer;
use crate::record_replication::{RecordAction, RecordAuthorizer};
use crate::share_session::ShareSession;
use tilo_lock::MutexRecover as _;

pub const MAX_RECORD_ACCESS_ENTRIES: usize = 4_096;
pub const MAX_RECORD_ACCESS_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecordAccessSessionId(pub Uuid);

/// Auditable identity/scope binding created at session registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordAccessBinding {
    pub session_id: RecordAccessSessionId,
    pub grant_id: Uuid,
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
    pub person: Ed25519PublicKey,
    pub device: Ed25519PublicKey,
    pub expires_at_ms: u64,
}

struct Entry {
    binding: RecordAccessBinding,
    sequence: u64,
    session: Arc<ShareSession>,
}

#[derive(Default)]
struct State {
    next_sequence: u64,
    entries: HashMap<RecordAccessSessionId, Entry>,
}

/// Process-local, bounded live record access registry.
pub struct RecordAccessRegistry {
    capacity: usize,
    ttl_ms: u64,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RecordAccessRegistryError {
    #[error("record access capacity must be between 1 and {MAX_RECORD_ACCESS_ENTRIES}")]
    InvalidCapacity,
    #[error("record access TTL must be between 1ms and {MAX_RECORD_ACCESS_TTL_MS}ms")]
    InvalidTtl,
}

impl RecordAccessRegistry {
    /// Construct a registry using the system monotonic clock.
    pub fn new(capacity: usize, ttl_ms: u64) -> Result<Self, RecordAccessRegistryError> {
        Self::with_clock(capacity, ttl_ms, Arc::new(MonotonicClock))
    }

    /// Construct a registry using an injected monotonic clock.
    pub fn with_clock(
        capacity: usize,
        ttl_ms: u64,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, RecordAccessRegistryError> {
        if capacity == 0 || capacity > MAX_RECORD_ACCESS_ENTRIES {
            return Err(RecordAccessRegistryError::InvalidCapacity);
        }
        if ttl_ms == 0 || ttl_ms > MAX_RECORD_ACCESS_TTL_MS {
            return Err(RecordAccessRegistryError::InvalidTtl);
        }
        Ok(Self {
            capacity,
            ttl_ms,
            clock,
            state: Mutex::new(State::default()),
        })
    }

    /// Register a previously authenticated and authorized record session.
    /// Registration records identity and scope; it grants no new authority.
    pub fn register(&self, session: Arc<ShareSession>) -> RecordAccessBinding {
        let now_ms = self.clock.now_ms();
        let actor = session.actor();
        let binding = RecordAccessBinding {
            session_id: RecordAccessSessionId(Uuid::new_v4()),
            grant_id: session.grant_id(),
            workspace_id: session.workspace_id(),
            record_id: session.record_id(),
            person: actor.person,
            device: actor.device,
            expires_at_ms: now_ms.saturating_add(self.ttl_ms),
        };

        let mut state = self.state.lock_recover();
        state
            .entries
            .retain(|_, entry| entry.binding.expires_at_ms > now_ms);
        while state.entries.len() >= self.capacity {
            let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.sequence)
                .map(|(id, _)| *id)
            else {
                break;
            };
            state.entries.remove(&oldest);
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        state.entries.insert(
            binding.session_id,
            Entry {
                binding,
                sequence,
                session,
            },
        );
        binding
    }

    /// Return exact record IDs currently readable by this certified
    /// person/device in this workspace.
    ///
    /// No hash is accepted by this API. Every candidate session is checked
    /// again through both `actor_is_certified` and `allows`, so grant or device
    /// revocation takes effect on the next lookup.
    pub fn readable_record_ids(
        &self,
        workspace_id: Uuid,
        peer: &AuthenticatedFsPeer,
    ) -> Vec<FileRecordId> {
        self.readable_record_ids_matching(
            workspace_id,
            Ed25519PublicKey::from_bytes(peer.device_public_key),
            Some(Ed25519PublicKey::from_bytes(peer.user_public_key)),
        )
    }

    /// Device-only lookup for transport callbacks that expose the certified
    /// endpoint/device key but not the earlier peer object. Person identity is
    /// never caller-supplied: it is recovered independently from each bound
    /// session, then rechecked through that session's live authorizer.
    pub fn readable_record_ids_for_device(
        &self,
        workspace_id: Uuid,
        device: Ed25519PublicKey,
    ) -> Vec<FileRecordId> {
        self.readable_record_ids_matching(workspace_id, device, None)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard is already confined to the block that collects candidates"
    )]
    fn readable_record_ids_matching(
        &self,
        workspace_id: Uuid,
        device: Ed25519PublicKey,
        person: Option<Ed25519PublicKey>,
    ) -> Vec<FileRecordId> {
        let now_ms = self.clock.now_ms();
        let candidates: Vec<_> = {
            let mut state = self.state.lock_recover();
            state
                .entries
                .retain(|_, entry| entry.binding.expires_at_ms > now_ms);
            state
                .entries
                .values()
                .filter(|entry| {
                    entry.binding.workspace_id == workspace_id
                        && entry.binding.device == device
                        && person.is_none_or(|person| entry.binding.person == person)
                })
                .map(|entry| {
                    (
                        entry.binding.session_id,
                        entry.binding,
                        Arc::clone(&entry.session),
                    )
                })
                .collect()
        };

        let mut authorized = Vec::new();
        let mut rejected = Vec::new();
        for (session_id, binding, session) in candidates {
            // Defend the registry/session association as well as rechecking
            // live authorization. A corrupted association must fail closed.
            let session_actor = session.actor();
            let binding_is_current = session.workspace_id() == binding.workspace_id
                && session.record_id() == binding.record_id
                && session.grant_id() == binding.grant_id
                && session_actor.person == binding.person
                && session_actor.device == binding.device;
            if binding_is_current
                && session.actor_is_certified(&session_actor)
                && session.allows(&session_actor, binding.record_id, RecordAction::Read)
            {
                authorized.push((session_id, binding));
            } else {
                rejected.push(session_id);
            }
        }

        // Recheck membership and registry TTL after potentially slow live
        // authority work. A concurrent remove/capacity eviction, or expiry
        // during authorization, must not leak a stale record ID.
        let final_now_ms = self.clock.now_ms();
        let mut state = self.state.lock_recover();
        for id in rejected {
            state.entries.remove(&id);
        }
        state
            .entries
            .retain(|_, entry| entry.binding.expires_at_ms > final_now_ms);
        let readable: BTreeSet<_> = authorized
            .into_iter()
            .filter_map(|(id, binding)| {
                state
                    .entries
                    .get(&id)
                    .filter(|entry| entry.binding == binding)
                    .map(|_| binding.record_id)
            })
            .collect();
        readable.into_iter().collect()
    }

    /// Remove one registered session.
    pub fn remove(&self, session_id: RecordAccessSessionId) -> bool {
        self.state
            .lock_recover()
            .entries
            .remove(&session_id)
            .is_some()
    }

    /// Return the number of currently live sessions.
    pub fn len(&self) -> usize {
        let now_ms = self.clock.now_ms();
        let mut state = self.state.lock_recover();
        state
            .entries
            .retain(|_, entry| entry.binding.expires_at_ms > now_ms);
        state.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use fs_protocol::{FileInviteSpec, FileInviteSubject, FilePermission, mint_file_invite};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    use super::*;
    use crate::peer_auth::AuthenticatedFsPeer;
    use crate::share_authority::ShareAuthority;
    use crate::share_session::{LiveActorCertification, ShareSession};
    use tilo_clock::ManualClock;

    const NOW_MS: u64 = 2_000;

    struct LiveCertification(AtomicBool);

    impl LiveActorCertification for LiveCertification {
        fn certification_is_current(&self, _peer: &AuthenticatedFsPeer, _now_ns: u64) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        issuer: SigningKey,
        issuer_public: Ed25519PublicKey,
        person: SigningKey,
        peer: AuthenticatedFsPeer,
        workspace: Uuid,
        authority: Arc<ShareAuthority>,
        live: Arc<LiveCertification>,
        clock: Arc<ManualClock>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let issuer = SigningKey::from_bytes(&[11; 32]);
            let person = SigningKey::from_bytes(&[12; 32]);
            let device = SigningKey::from_bytes(&[13; 32]);
            let mut cert = DeviceCert {
                user_public_key: person.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1_000_000_000,
                expires_at_ns: Some(20_000_000_000),
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &person).unwrap();
            let peer = AuthenticatedFsPeer {
                user_public_key: cert.user_public_key,
                device_public_key: cert.device_public_key,
                device_label: cert.device_label.clone(),
                capabilities: cert.capabilities.clone(),
                cert,
            };
            let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
            let authority = Arc::new(ShareAuthority::open(dir.path().join("shares.json")).unwrap());
            Self {
                _dir: dir,
                issuer,
                issuer_public,
                person,
                peer,
                workspace: Uuid::new_v4(),
                authority,
                live: Arc::new(LiveCertification(AtomicBool::new(true))),
                clock: Arc::new(ManualClock::new(NOW_MS)),
            }
        }

        fn session(&self, record_id: FileRecordId) -> Arc<ShareSession> {
            let token = mint_file_invite(
                FileInviteSpec {
                    workspace_id: self.workspace,
                    record_id,
                    subject: FileInviteSubject::Person(Ed25519PublicKey::from_bytes(
                        self.person.verifying_key().to_bytes(),
                    )),
                    permission: FilePermission::View,
                    expires_at_ms: Some(10_000),
                    max_redemptions: None,
                },
                &self.issuer,
            )
            .unwrap();
            self.authority
                .register(&token, &[self.issuer_public], NOW_MS)
                .unwrap();
            Arc::new(
                ShareSession::open_named_with_clock(
                    self.workspace,
                    self.peer.clone(),
                    token,
                    self.authority.clone(),
                    self.live.clone(),
                    self.clock.clone(),
                )
                .unwrap(),
            )
        }

        fn registry(&self, capacity: usize, ttl_ms: u64) -> Arc<RecordAccessRegistry> {
            RecordAccessRegistry::with_clock(capacity, ttl_ms, self.clock.clone())
                .map(Arc::new)
                .unwrap()
        }
    }

    #[test]
    fn record_lookups_return_exact_records_only_to_the_current_bound_subject() {
        {
            let fixture = Fixture::new();
            let registry = fixture.registry(4, 1_000);
            let first = FileRecordId::new();
            let second = FileRecordId::new();
            registry.register(fixture.session(first));
            registry.register(fixture.session(second));
            assert_eq!(
                registry.readable_record_ids(fixture.workspace, &fixture.peer),
                vec![first, second]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            );
        }
        {
            let fixture = Fixture::new();
            let registry = fixture.registry(4, 1_000);
            let record = FileRecordId::new();
            let binding = registry.register(fixture.session(record));
            fixture.authority.revoke(binding.grant_id).unwrap();
            assert!(
                registry
                    .readable_record_ids(fixture.workspace, &fixture.peer)
                    .is_empty()
            );

            let record = FileRecordId::new();
            registry.register(fixture.session(record));
            fixture.live.0.store(false, Ordering::SeqCst);
            assert!(
                registry
                    .readable_record_ids(fixture.workspace, &fixture.peer)
                    .is_empty()
            );
        }
        {
            let fixture = Fixture::new();
            let registry = fixture.registry(2, 1_000);
            registry.register(fixture.session(FileRecordId::new()));
            let mut wrong_device = fixture.peer.clone();
            wrong_device.device_public_key = [99; 32];
            assert!(
                registry
                    .readable_record_ids(fixture.workspace, &wrong_device)
                    .is_empty()
            );
            let mut wrong_person = fixture.peer.clone();
            wrong_person.user_public_key = [98; 32];
            assert!(
                registry
                    .readable_record_ids(fixture.workspace, &wrong_person)
                    .is_empty()
            );
            assert!(
                registry
                    .readable_record_ids(Uuid::new_v4(), &fixture.peer)
                    .is_empty()
            );
        }
        {
            let fixture = Fixture::new();
            let registry = fixture.registry(2, 1_000);
            let record = FileRecordId::new();
            let binding = registry.register(fixture.session(record));
            let device = Ed25519PublicKey::from_bytes(fixture.peer.device_public_key);
            assert_eq!(
                registry.readable_record_ids_for_device(fixture.workspace, device),
                vec![record]
            );
            assert!(
                registry
                    .readable_record_ids_for_device(
                        fixture.workspace,
                        Ed25519PublicKey::from_bytes([77; 32]),
                    )
                    .is_empty()
            );
            fixture.authority.revoke(binding.grant_id).unwrap();
            assert!(
                registry
                    .readable_record_ids_for_device(fixture.workspace, device)
                    .is_empty()
            );
        }
    }

    #[test]
    fn the_session_registry_is_bounded_expiring_and_validated() {
        {
            let fixture = Fixture::new();
            let registry = fixture.registry(2, 1_000);
            let first = FileRecordId::new();
            let second = FileRecordId::new();
            let third = FileRecordId::new();
            let first_binding = registry.register(fixture.session(first));
            registry.register(fixture.session(second));
            registry.register(fixture.session(third));
            assert_eq!(registry.len(), 2);
            assert!(!registry.remove(first_binding.session_id));
            let readable = registry.readable_record_ids(fixture.workspace, &fixture.peer);
            assert!(!readable.contains(&first));
            assert!(readable.contains(&second));
            assert!(readable.contains(&third));
        }
        {
            let fixture = Fixture::new();
            let registry = fixture.registry(2, 50);
            registry.register(fixture.session(FileRecordId::new()));
            fixture.clock.set(NOW_MS + 50);
            assert!(
                registry
                    .readable_record_ids(fixture.workspace, &fixture.peer)
                    .is_empty()
            );
            assert!(registry.is_empty());
        }
        {
            let fixture = Arc::new(Fixture::new());
            let registry = fixture.registry(8, 1_000);
            let mut threads = Vec::new();
            for _ in 0..24 {
                let fixture = fixture.clone();
                let registry = registry.clone();
                threads.push(thread::spawn(move || {
                    registry.register(fixture.session(FileRecordId::new()));
                    let _ = registry.readable_record_ids(fixture.workspace, &fixture.peer);
                }));
            }
            for thread in threads {
                thread.join().unwrap();
            }
            assert!(registry.len() <= 8);
            assert!(
                registry
                    .readable_record_ids(fixture.workspace, &fixture.peer)
                    .len()
                    <= 8
            );
        }
        {
            assert!(matches!(
                RecordAccessRegistry::new(0, 1),
                Err(RecordAccessRegistryError::InvalidCapacity)
            ));
            assert!(matches!(
                RecordAccessRegistry::new(MAX_RECORD_ACCESS_ENTRIES + 1, 1),
                Err(RecordAccessRegistryError::InvalidCapacity)
            ));
            assert!(matches!(
                RecordAccessRegistry::new(1, 0),
                Err(RecordAccessRegistryError::InvalidTtl)
            ));
        }
    }
}
