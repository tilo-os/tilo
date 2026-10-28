//! Remote actions count as use.
//!
//! The idle-suspend timer only sees local input, so a person working on this
//! machine from another of their devices would watch it fall asleep mid-task.
//! Every action that arrives from one of the person's own devices renews a
//! suspend-only lease in the session's [`InhibitRegistry`]: the screen still
//! blanks and locks, lid close and critical battery still suspend, and once
//! the requests stop the lease lapses by itself.

use std::time::{Duration, Instant};

use tilo_power_mgmt::inhibit::INHIBIT_SUSPEND;
use tilo_power_mgmt::{InhibitRegistry, InhibitToken};

/// How long one remote action keeps this machine from idle-suspending.
pub const REMOTE_USE_LEASE: Duration = Duration::from_secs(10 * 60);

/// What one renewal did to the lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renewal {
    /// No lease was running: a new hold starts now.
    Taken,
    /// The running lease now ends later.
    Extended,
    /// The session has no inhibitor registry (not a seat-owning desktop).
    Unattached,
}

/// The one suspend lease remote use holds in this session.
#[derive(Debug, Default)]
pub struct RemoteUseLease {
    registry: Option<InhibitRegistry>,
    hold: Option<InhibitToken>,
    until: Option<Instant>,
}

impl RemoteUseLease {
    /// Hold leases in `registry`, the one the session's idle loop consults.
    pub fn attach(&mut self, registry: InhibitRegistry) {
        self.hold = None;
        self.until = None;
        self.registry = Some(registry);
    }

    /// A remote action from `source_label` arrived at `now`: hold suspend
    /// off until [`REMOTE_USE_LEASE`] after it.
    pub fn renew(&mut self, source_label: &str, now: Instant) -> Renewal {
        let Some(registry) = self.registry.as_ref() else {
            return Renewal::Unattached;
        };
        let until = now.checked_add(REMOTE_USE_LEASE).unwrap_or(now);
        let reason = reason(source_label);
        let running = self.until.is_some_and(|deadline| now < deadline);
        let renewed = self
            .hold
            .as_ref()
            .is_some_and(|hold| hold.renew_until(reason.clone(), until));
        if !renewed {
            self.hold = Some(registry.acquire_until(reason, INHIBIT_SUSPEND, until));
        }
        self.until = Some(until);
        if running && renewed {
            Renewal::Extended
        } else {
            Renewal::Taken
        }
    }

    /// When the lease lapses, if one was ever taken.
    #[must_use]
    pub const fn until(&self) -> Option<Instant> {
        self.until
    }
}

/// The reason the lease shows in the registry and in logind's inhibitor list.
#[must_use]
pub fn reason(source_label: &str) -> String {
    format!("Remote use from {source_label}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_power_mgmt::inhibit::INHIBIT_IDLE;

    #[test]
    fn remote_use_holds_suspend_for_a_lease_after_the_last_action() {
        let registry = InhibitRegistry::new();
        let mut lease = RemoteUseLease::default();
        let start = Instant::now();
        assert_eq!(
            lease.renew("Laptop", start),
            Renewal::Unattached,
            "a session without a registry holds nothing"
        );

        lease.attach(registry.clone());
        assert_eq!(lease.renew("Laptop", start), Renewal::Taken);
        assert!(registry.is_inhibited_for_at(INHIBIT_SUSPEND, start));
        assert!(
            !registry.is_inhibited_for_at(INHIBIT_IDLE, start),
            "the screen still blanks and locks"
        );
        assert_eq!(
            registry.active_reasons_for(INHIBIT_SUSPEND),
            vec!["Remote use from Laptop".to_string()]
        );

        let later = start + Duration::from_secs(9 * 60);
        assert_eq!(lease.renew("Phone", later), Renewal::Extended);
        assert!(registry.is_inhibited_for_at(INHIBIT_SUSPEND, start + REMOTE_USE_LEASE));
        assert_eq!(
            registry.active_reasons_for(INHIBIT_SUSPEND),
            vec!["Remote use from Phone".to_string()]
        );
        assert_eq!(registry.count(), 1, "one lease, renewed in place");

        let lapsed = later + REMOTE_USE_LEASE;
        assert!(!registry.is_inhibited_for_at(INHIBIT_SUSPEND, lapsed));
        assert_eq!(lease.until(), Some(lapsed));

        // The next action after it lapsed takes a fresh hold.
        assert_eq!(lease.renew("Laptop", lapsed), Renewal::Taken);
        assert!(registry.is_inhibited_for_at(INHIBIT_SUSPEND, lapsed));
    }
}
