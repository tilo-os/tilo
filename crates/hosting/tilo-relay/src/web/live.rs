//! The host's [`LiveAuthority`] for browser live sessions (WebTransport
//! `moq-lite`, or its HTTPS fallback).
//!
//! A grant is issued to an authenticated browser session for one workspace
//! it may subscribe to, and is presented in the carrier's admission preface.
//! Permission is observed for every grant together (PR 541's batched
//! observation): the session is still open, the device is not revoked, and it
//! is still admitted to the workspace. [`LiveGrants::follow`] observes when
//! one of those may have changed — an announced authority change
//! ([`crate::authority_epoch`]), a session ending or running out, a grant
//! issued — and on the janitor's sweep as a backstop, not on a fixed clock.
//! The relay reads each grant's observed permission on every per-session
//! recheck, so a revoked or removed device loses its live session within the
//! relay's recheck interval of the change, and no recheck ever waits on the
//! workspace locks.
//!
//! Frames are checked with the host's canonical codec for sealed live frames
//! ([`tilo_relay_protocol::live::SealedLiveFrame`]): only their clear kind,
//! sequence and record are read, never their content.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_moq::live::{AuthorizationStatus, Denied, LiveAuthority, LiveSession};
use tilo_relay_protocol::live::{LiveKind, SealedLiveFrame};
use tilo_relay_protocol::web::{LiveGrant, agent_publisher, live_room};
use tokio::sync::Notify;

use crate::auth::AuthenticatedPeer;
use crate::room_access::{self, RoomOperation};
use crate::web::sessions::Sessions;

// The live relay (`tilo-moq`, which stays free of the host's wire crate) and
// the host's canonical live codec must agree on every frame and rate limit.
const _: () =
    assert!(tilo_moq::live::MAX_PRESENCE_RATE == tilo_relay_protocol::live::MAX_PRESENCE_RATE);
const _: () = assert!(tilo_moq::live::MAX_OPS_RATE == tilo_relay_protocol::live::MAX_OPS_RATE);
const _: () = assert!(
    tilo_moq::live::MAX_DATAGRAM_FRAME_BYTES == tilo_relay_protocol::live::MAX_PRESENCE_FRAME_BYTES
);
const _: () = assert!(
    tilo_moq::live::MAX_STREAM_FRAME_BYTES == tilo_relay_protocol::live::MAX_OPS_FRAME_BYTES
);

/// How often idle sessions are swept and, with them, every grant observed
/// even though nothing announced a change.
const SWEEP: Duration = Duration::from_secs(5);
/// Grant lifetime; the page asks for a new one before it ends.
const GRANT_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_GRANTS: usize = 512;

struct Grant {
    session: [u8; 32],
    device: [u8; 32],
    workspace_id: u64,
    room: String,
    publisher: String,
    expires: Instant,
    permitted: AtomicBool,
}

/// Grants and their observed permission.
pub(crate) struct LiveGrants {
    grants: Mutex<HashMap<[u8; 32], Arc<Grant>>>,
    /// Signalled when a grant is issued: its admission may predate a change
    /// an observation already ran for.
    issued: Notify,
}

impl LiveGrants {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            grants: Mutex::default(),
            issued: Notify::new(),
        })
    }

    /// Observe every grant whenever its permission may have changed, and
    /// sweep `sessions` every [`SWEEP`]. Runs until aborted.
    pub(crate) async fn follow(self: Arc<Self>, sessions: Arc<Sessions>) {
        let mut authority = crate::authority_epoch::subscribe();
        let mut sweep = tokio::time::interval(SWEEP);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // A session that runs out is no longer open, and nothing
            // announces it.
            let expiry = sessions.next_expiry().map(tokio::time::Instant::from_std);
            tokio::select! {
                // The epoch's sender is a process-wide static and never
                // closes, so this cannot spin on an error.
                _ = authority.changed() => {}
                () = sessions.ended() => {}
                () = self.issued.notified() => {}
                () = tokio::time::sleep_until(expiry.unwrap_or_else(tokio::time::Instant::now)), if expiry.is_some() => {}
                _ = sweep.tick() => sessions.sweep(),
            }
            self.observe(&sessions).await;
        }
    }

    /// Issue a grant for `workspace_id` to the session `token` of `peer`, if
    /// it is admitted to the room's live plane now.
    /// With `agent`, the grant publishes as that agent of the device
    /// ([`agent_publisher`]).
    pub(crate) async fn issue(
        &self,
        sessions: &Sessions,
        token: [u8; 32],
        peer: &AuthenticatedPeer,
        workspace_id: u64,
        agent: Option<&str>,
    ) -> Option<LiveGrant> {
        if !admitted(sessions, peer, workspace_id).await {
            return None;
        }
        let device = peer.device_public_key;
        let grant_token: [u8; 32] = rand::random();
        let room = live_room(workspace_id);
        let publisher = agent.map_or_else(
            || hex::encode(device),
            |agent| hex::encode(agent_publisher(&room, device, agent)),
        );
        let grant = Arc::new(Grant {
            session: token,
            device,
            workspace_id,
            room,
            publisher,
            expires: Instant::now()
                .checked_add(GRANT_TTL)
                .unwrap_or_else(Instant::now),
            permitted: AtomicBool::new(true),
        });
        {
            let mut grants = self
                .grants
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            grants.retain(|_, grant| grant.expires > Instant::now());
            if grants.len() >= MAX_GRANTS {
                return None;
            }
            grants.insert(grant_token, grant.clone());
        }
        self.issued.notify_one();
        Some(LiveGrant {
            token: grant_token.to_vec(),
            room: grant.room.clone(),
            publisher: grant.publisher.clone(),
            expires_at_ns: tilo_clock::unix_now_ns()
                .saturating_add(u64::try_from(GRANT_TTL.as_nanos()).unwrap_or(u64::MAX)),
        })
    }

    /// The workspace a live room (64 hex) belongs to, while a grant for it
    /// is held.
    pub(crate) fn workspace_of(&self, room: &str) -> Option<u64> {
        self.grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .find(|grant| grant.room == room)
            .map(|grant| grant.workspace_id)
    }

    /// The device that publishes as itself under `publisher` in `room`;
    /// `None` for an agent's publisher.
    pub(crate) fn publisher_device(&self, room: &str, publisher: &str) -> Option<[u8; 32]> {
        self.grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .find(|grant| grant.room == room && grant.publisher == publisher)
            .filter(|grant| grant.publisher == hex::encode(grant.device))
            .map(|grant| grant.device)
    }

    /// Observe every grant once.
    pub(crate) async fn observe(&self, sessions: &Sessions) {
        let grants: Vec<Arc<Grant>> = self
            .grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let open: HashMap<[u8; 32], AuthenticatedPeer> =
            sessions.open_devices().into_iter().collect();
        for grant in grants {
            let permitted = grant.expires > Instant::now()
                && match open.get(&grant.session) {
                    Some(peer) if peer.device_public_key == grant.device => {
                        admitted(sessions, peer, grant.workspace_id).await
                    }
                    Some(_) | None => false,
                };
            // Update only grants this observation checked. A grant issued
            // while admission awaits a workspace lock keeps its own decision.
            grant.permitted.store(permitted, Ordering::Release);
        }
    }
}

/// Whether `peer` may be in `workspace_id`'s live plane now: its
/// certificate current relay-wide and admitted by the room, see
/// [`room_access::decide`].
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
pub(crate) async fn admitted(
    sessions: &Sessions,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
) -> bool {
    let relay = &sessions.context().relay;
    let workspace = relay.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(workspace) = workspace else {
        return false;
    };
    let workspace = workspace.lock().await;
    room_access::decide(&workspace, &relay.trust(), peer, RoomOperation::Live).admitted()
}

struct Admitted {
    grant: Arc<Grant>,
}

impl LiveSession for Admitted {
    fn room(&self) -> &str {
        &self.grant.room
    }
    fn publisher(&self) -> &str {
        &self.grant.publisher
    }
    fn publish(&self) -> bool {
        true
    }
    fn subscribe(&self) -> bool {
        true
    }
    fn status(&self) -> AuthorizationStatus {
        let permitted = self.grant.permitted.load(Ordering::Acquire);
        if permitted && self.grant.expires > Instant::now() {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }
}

/// [`LiveAuthority`] over [`LiveGrants`].
pub(crate) struct Authority(pub(crate) Arc<LiveGrants>);

impl LiveAuthority for Authority {
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Denied> {
        let token: [u8; 32] = token.try_into().map_err(|_| Denied)?;
        let grant = self
            .0
            .grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .cloned()
            .ok_or(Denied)?;
        let session = Admitted { grant };
        match session.status() {
            AuthorizationStatus::Current => Ok(Arc::new(session)),
            AuthorizationStatus::Denied => Err(Denied),
        }
    }

    fn presence_seq(&self, frame: &[u8]) -> Option<u64> {
        match SealedLiveFrame::decode(frame) {
            Ok(sealed) if sealed.kind == LiveKind::Presence => Some(sealed.seq),
            _ => None,
        }
    }

    fn ops_valid(&self, record: &str, frame: &[u8]) -> bool {
        matches!(
            SealedLiveFrame::decode(frame),
            Ok(sealed) if sealed.kind == LiveKind::Ops && hex::encode(sealed.record) == record
        )
    }
}

#[cfg(test)]
mod tests {
    use tilo_relay_protocol::live::{LiveFrame, LiveRecord, OpsFrame, PresenceFrame};

    use super::*;

    #[test]
    fn expired_live_grants_are_denied_before_the_next_observation() {
        let grants = LiveGrants::new();
        let grant = Arc::new(Grant {
            session: [1; 32],
            device: [2; 32],
            workspace_id: 3,
            room: live_room(3),
            publisher: hex::encode([2; 32]),
            expires: Instant::now(),
            permitted: AtomicBool::new(true),
        });
        grants.grants.lock().unwrap().insert([4; 32], grant.clone());
        let held = Admitted { grant };
        assert!(matches!(held.status(), AuthorizationStatus::Denied));
        assert!(matches!(Authority(grants).try_admit(&[4; 32]), Err(Denied)));
    }

    fn record() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: "r1".into(),
        }
    }

    fn sealed(kind: LiveKind, seq: u64) -> Vec<u8> {
        SealedLiveFrame {
            kind,
            seq,
            record: record().digest(),
            channel: [7; 16],
            epoch: 1,
            payload: vec![0x5a; 40],
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn the_browser_live_plane_reads_only_the_clear_tags_of_sealed_frames() {
        let authority = Authority(LiveGrants::new());
        let track = hex::encode(record().digest());

        assert_eq!(
            authority.presence_seq(&sealed(LiveKind::Presence, 4)),
            Some(4)
        );
        assert_eq!(authority.presence_seq(&sealed(LiveKind::Ops, 4)), None);
        assert!(authority.ops_valid(&track, &sealed(LiveKind::Ops, 2)));
        assert!(!authority.ops_valid(&hex::encode([1; 32]), &sealed(LiveKind::Ops, 2)));
        assert!(!authority.ops_valid(&track, &sealed(LiveKind::Presence, 2)));

        // The same frames in the clear are refused.
        let presence = LiveFrame::Presence(PresenceFrame {
            seq: 4,
            sent_at_ms: 1,
            focus: Some(record()),
            pointer: Some([1.0, 2.0]),
            selection: None,
            drag: None,
            participant: None,
        })
        .encode()
        .unwrap();
        let end = LiveFrame::Ops(OpsFrame::End { record: record() })
            .encode()
            .unwrap();
        assert_eq!(authority.presence_seq(&presence), None);
        assert!(!authority.ops_valid(&track, &end));
    }
}
