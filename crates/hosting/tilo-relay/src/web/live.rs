//! The host's [`LiveAuthority`] for browser live sessions (WebTransport
//! `moq-lite`, or its HTTPS fallback).
//!
//! A grant is issued to an authenticated browser session for one workspace
//! it may subscribe to, and is presented in the carrier's admission preface.
//! Permission is observed once per [`OBSERVATION`] for every grant together
//! (PR 541's batched observation): the session is still open, the device is
//! not revoked, and it is still admitted to the workspace. The relay reads
//! that snapshot on every per-session recheck, so a revoked or removed
//! device loses its live session within one observation plus the relay's
//! recheck interval, and no recheck ever waits on the workspace locks.
//!
//! Frames are checked with the host's canonical codec for sealed live frames
//! ([`tilo_relay_protocol::live::SealedLiveFrame`]): only their clear kind,
//! sequence and record are read, never their content.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tilo_moq::live::{Admission, AuthorizationStatus, LiveAuthority, LiveSession};
use tilo_relay_protocol::live::{LiveKind, SealedLiveFrame};
use tilo_relay_protocol::web::{LiveGrant, agent_publisher, live_room};

use crate::web::sessions::Sessions;

/// How often grant permission is observed.
pub(crate) const OBSERVATION: Duration = Duration::from_millis(50);
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
}

/// Grants and their observed permission.
pub(crate) struct LiveGrants {
    grants: Mutex<HashMap<[u8; 32], Arc<Grant>>>,
    permitted: RwLock<HashMap<[u8; 32], bool>>,
}

impl LiveGrants {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            grants: Mutex::default(),
            permitted: RwLock::default(),
        })
    }

    /// Issue a grant for `workspace_id` to the session `token`, if its
    /// device is subscribed-capable there now.
    /// With `agent`, the grant publishes as that agent of the device
    /// ([`agent_publisher`]).
    pub(crate) async fn issue(
        &self,
        sessions: &Sessions,
        token: [u8; 32],
        device: [u8; 32],
        user: [u8; 32],
        workspace_id: u64,
        agent: Option<&str>,
    ) -> Option<LiveGrant> {
        if !admitted(sessions, device, user, workspace_id).await {
            return None;
        }
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
        self.permitted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(grant_token, true);
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
        let grants: Vec<([u8; 32], Arc<Grant>)> = self
            .grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(token, grant)| (*token, grant.clone()))
            .collect();
        let open: HashMap<[u8; 32], [u8; 32]> = sessions
            .open_devices()
            .into_iter()
            .map(|(token, peer)| (token, peer.user_public_key))
            .collect();
        let mut next = HashMap::with_capacity(grants.len());
        for (token, grant) in grants {
            let permitted = grant.expires > Instant::now()
                && match open.get(&grant.session) {
                    Some(user) => admitted(sessions, grant.device, *user, grant.workspace_id).await,
                    None => false,
                };
            next.insert(token, permitted);
        }
        *self
            .permitted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }
}

/// Whether `device` of `user` may be in `workspace_id`'s live plane now:
/// not revoked and admitted exactly as a subscribe would be.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
pub(crate) async fn admitted(
    sessions: &Sessions,
    device: [u8; 32],
    user: [u8; 32],
    workspace_id: u64,
) -> bool {
    let relay = &sessions.context().relay;
    if relay
        .revocations
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_revoked(&device)
    {
        return false;
    }
    let workspace = relay.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(workspace) = workspace else {
        return false;
    };
    let workspace = workspace.lock().await;
    workspace.admit_authenticated
        || workspace.membership.contains(&device)
        || workspace.admitted_users.contains(&user)
}

struct Admitted {
    token: [u8; 32],
    grant: Arc<Grant>,
    grants: Arc<LiveGrants>,
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
        let permitted = self
            .grants
            .permitted
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.token)
            .copied()
            .unwrap_or(false);
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
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Admission> {
        let token: [u8; 32] = token.try_into().map_err(|_| Admission::Denied)?;
        let grant = self
            .0
            .grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .cloned()
            .ok_or(Admission::Denied)?;
        let session = Admitted {
            token,
            grant,
            grants: self.0.clone(),
        };
        match session.status() {
            AuthorizationStatus::Current => Ok(Arc::new(session)),
            _ => Err(Admission::Denied),
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
