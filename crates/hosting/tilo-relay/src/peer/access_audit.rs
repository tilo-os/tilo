//! When a connection's access is next audited.
//!
//! The audit ([`super::audit_subscriber_access`]) is what ends a session
//! whose device was revoked, removed from a room, displaced by a newer
//! connection, or whose certificate or agent delegation ran out. It runs
//! when one of those may have happened rather than on a fixed clock:
//!
//! - [`AUDIT_SETTLE`] after anything it reads announces a change
//!   ([`crate::authority_epoch`]);
//! - [`AUDIT_SETTLE`] after the certificate or agent delegation the
//!   connection holds expires (every frame is already refused from the
//!   expiry on; the settle lets one in flight be answered before the close);
//! - every [`UNWATCHED_AUDIT`] while a subscribed room's membership file can
//!   only be observed by reading it (no live membership watch covers it);
//! - otherwise after [`AUDIT_BACKSTOP`], for a writer that forgets to
//!   announce its change.
//!
//! An idle host therefore wakes its connections only when something changed,
//! not once per connection per second.

use std::time::Duration;

use tilo_clock::unix_now_ns;
use tokio::time::Instant;

use super::AgentRegistry;
use crate::auth::AuthenticatedPeer;
use tilo_lock::RwLockRecover as _;

/// Audit cadence while a subscribed room's membership file is not watched:
/// reading the disk is then the only way an external edit is seen, so it is
/// read as often as before the watch existed.
const UNWATCHED_AUDIT: Duration = Duration::from_secs(1);
/// The longest a connection goes unaudited while every input of its audit
/// announces its changes.
const AUDIT_BACKSTOP: Duration = Duration::from_secs(30);
/// How long an announced change waits for its audit. It coalesces a burst of
/// changes into one audit, and lets a frame already in flight when access
/// narrowed be refused with `NotAuthorised` rather than meet a bare close.
const AUDIT_SETTLE: Duration = Duration::from_millis(100);

/// One connection's audit schedule.
pub(super) struct AccessAudit {
    due: Instant,
    /// The subscribed rooms the last audit found change-notified.
    watched: Vec<u64>,
}

impl AccessAudit {
    /// A new connection is first audited on the unwatched cadence: until an
    /// audit has looked, nothing is known about its rooms.
    pub(super) fn new() -> Self {
        Self {
            due: after(Instant::now(), UNWATCHED_AUDIT),
            watched: Vec::new(),
        }
    }

    /// When the next audit is due.
    pub(super) const fn due(&self) -> Instant {
        self.due
    }

    /// Something an audit reads announced a change.
    pub(super) fn changed(&mut self) {
        self.due = self.due.min(after(Instant::now(), AUDIT_SETTLE));
    }

    /// A room subscribed since the last audit is not known to be watched.
    pub(super) fn cover(&mut self, subscribed: &[u64]) {
        if subscribed.iter().any(|room| !self.watched.contains(room)) {
            self.due = self.due.min(after(Instant::now(), UNWATCHED_AUDIT));
        }
    }

    /// Schedule the next audit after one that passed; `watched` says whether
    /// every room in `subscribed` had change-notified membership.
    pub(super) fn audited(
        &mut self,
        peer: &AuthenticatedPeer,
        agents: &AgentRegistry,
        subscribed: &[u64],
        watched: bool,
    ) {
        let now = Instant::now();
        let mut due = after(
            now,
            if watched {
                AUDIT_BACKSTOP
            } else {
                UNWATCHED_AUDIT
            },
        );
        let delegation_expiry = agents
            .read_recover()
            .get(&peer.device_public_key)
            .and_then(|delegation| delegation.as_ref().map(|live| live.expires_at_ns));
        // Only a future expiry is a deadline. One already past was judged by
        // the audit that just passed (a connection without rooms holds no
        // access to lose), and must not schedule an audit loop.
        let now_ns = unix_now_ns();
        for expiry in [peer.cert.expires_at_ns, delegation_expiry]
            .into_iter()
            .flatten()
            .filter(|expiry| *expiry > now_ns)
        {
            due = due.min(after(
                now,
                Duration::from_nanos(expiry.saturating_sub(now_ns)).saturating_add(AUDIT_SETTLE),
            ));
        }
        self.due = due;
        self.watched = if watched {
            subscribed.to_vec()
        } else {
            Vec::new()
        };
    }
}

/// `delay` after `now`; `now` itself should the clock overflow, which audits
/// at once rather than never.
fn after(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
    use tilo_user_identity::UserIdentity;

    use super::*;
    use crate::peer::private_transport_tests::certified_device;

    fn peer() -> (ed25519_dalek::SigningKey, AuthenticatedPeer) {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (peer, _) = certified_device(&root, user.public_key, "laptop");
        (root, peer)
    }

    fn in_secs(seconds: u64) -> u64 {
        unix_now_ns().saturating_add(seconds.saturating_mul(1_000_000_000))
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_watched_connection_waits_for_the_backstop_not_a_tick() {
        let (_, peer) = peer();
        let agents = AgentRegistry::default();
        let mut audit = AccessAudit::new();
        assert_eq!(audit.due(), Instant::now() + UNWATCHED_AUDIT);

        audit.audited(&peer, &agents, &[7], true);
        audit.cover(&[7]);
        assert_eq!(audit.due(), Instant::now() + AUDIT_BACKSTOP);

        // Ten idle seconds later nothing is due yet.
        tokio::time::advance(Duration::from_secs(10)).await;
        audit.cover(&[7]);
        assert!(audit.due() > Instant::now());
    }

    #[tokio::test(start_paused = true)]
    async fn an_announced_change_is_audited_after_the_settle() {
        let (_, peer) = peer();
        let agents = AgentRegistry::default();
        let mut audit = AccessAudit::new();
        audit.audited(&peer, &agents, &[7], true);
        audit.changed();
        assert_eq!(audit.due(), Instant::now() + AUDIT_SETTLE);
        // A second change does not push the audit back.
        tokio::time::advance(Duration::from_millis(60)).await;
        audit.changed();
        assert_eq!(
            audit.due(),
            Instant::now() + AUDIT_SETTLE - Duration::from_millis(60)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unwatched_and_newly_subscribed_rooms_keep_the_one_second_cadence() {
        let (_, peer) = peer();
        let agents = AgentRegistry::default();
        let mut audit = AccessAudit::new();

        audit.audited(&peer, &agents, &[7], false);
        assert_eq!(audit.due(), Instant::now() + UNWATCHED_AUDIT);
        audit.cover(&[7]);
        assert_eq!(audit.due(), Instant::now() + UNWATCHED_AUDIT);

        audit.audited(&peer, &agents, &[7], true);
        audit.cover(&[7, 8]);
        assert_eq!(audit.due(), Instant::now() + UNWATCHED_AUDIT);
    }

    #[tokio::test(start_paused = true)]
    async fn certificate_and_delegation_expiry_are_deadlines() {
        let (root, mut peer) = peer();
        let agents = AgentRegistry::default();
        let mut audit = AccessAudit::new();

        peer.cert.expires_at_ns = Some(in_secs(5));
        audit.audited(&peer, &agents, &[7], true);
        assert!(audit.due() <= Instant::now() + Duration::from_secs(5) + AUDIT_SETTLE);
        assert!(audit.due() > Instant::now() + Duration::from_secs(4));

        let delegation = Delegation::sign(
            &root,
            peer.device_public_key,
            tilo_relay_protocol::delegation::Delegate::Agent {
                agent_id: "helper".into(),
            },
            "Helper",
            [1, 2, 3],
            DelegateScope {
                rooms: vec![uuid::Uuid::from_u128(7).to_string()],
                apps: Vec::new(),
            },
            1,
            in_secs(2),
        )
        .unwrap();
        agents
            .write()
            .unwrap()
            .insert(peer.device_public_key, Some(delegation));
        audit.audited(&peer, &agents, &[7], true);
        assert!(audit.due() <= Instant::now() + Duration::from_secs(2) + AUDIT_SETTLE);
        assert!(audit.due() > Instant::now() + Duration::from_secs(1));

        // An expiry already past is not a deadline to spin on.
        agents.write().unwrap().clear();
        peer.cert.expires_at_ns = Some(1);
        audit.audited(&peer, &agents, &[], true);
        assert_eq!(audit.due(), Instant::now() + AUDIT_BACKSTOP);
    }
}
