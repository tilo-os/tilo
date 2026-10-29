//! The one durable phase machine of a Home capture attempt. Both routes that
//! run the confined original reader record their progress here: the capture
//! onto the admitted installer medium, and the copy from the original into an
//! installed Tilo. They share every phase they have in common, and each route
//! declares the transitions it may take; a record never moves outside them.
//! A phase is a hint to revalidate, never authority over the output.
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Phase {
    /// The intent is recorded; nothing has been launched.
    Prepared,
    /// The private target scratch is bound (installed copy).
    ScratchReady,
    /// A worker may be running; an unknown launch stays quarantined.
    Launching,
    /// The worker reported an export; nothing is verified yet.
    Exported,
    /// The independent verifier may be running.
    Verifying,
    /// Verified artifacts are being sealed for the receiver (installed copy).
    Sealing,
    /// A stopped attempt is being recovered from its sealed artifacts.
    RecoveryPrepared,
    /// The recovery verifier may be running.
    RecoveryVerifying,
    /// The sealed artifacts wait for the private importer (installed copy).
    ReceiverPending,
    /// The person or the importer stopped the attempt.
    Cancelled,
    /// The output is verified and accounted for.
    Verified,
}

/// Which route an attempt belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Captured onto the admitted installer medium before installation.
    Medium,
    /// Copied from the original into the installed Tilo.
    InstalledCopy,
}

impl Route {
    /// Every transition this route's attempts may take.
    fn transitions(self) -> &'static [(Phase, Phase)] {
        use Phase::*;
        match self {
            Self::Medium => &[
                (Prepared, Launching),
                (Launching, Exported),
                (Exported, Verifying),
                (Verifying, Verified),
            ],
            Self::InstalledCopy => &[
                (Prepared, ScratchReady),
                (ScratchReady, Launching),
                (Launching, Exported),
                (Exported, Verifying),
                (Verifying, Sealing),
                (RecoveryVerifying, Sealing),
                (Sealing, ReceiverPending),
                (ReceiverPending, Verified),
                (Launching, Cancelled),
                (Exported, Cancelled),
                (Verifying, Cancelled),
                (Sealing, Cancelled),
                (ReceiverPending, Cancelled),
                (Sealing, RecoveryPrepared),
                (ReceiverPending, RecoveryPrepared),
                (Cancelled, RecoveryPrepared),
                (RecoveryPrepared, RecoveryPrepared),
                (RecoveryVerifying, RecoveryPrepared),
                (RecoveryPrepared, RecoveryVerifying),
            ],
        }
    }

    /// Whether a record of this route may hold `phase` at all.
    pub(super) fn admits(self, phase: Phase) -> bool {
        phase == Phase::Prepared
            || self
                .transitions()
                .iter()
                .any(|(from, to)| *from == phase || *to == phase)
    }
}

impl Phase {
    /// Move to `to`, refusing any transition `route` does not declare.
    pub(super) fn advance(&mut self, route: Route, to: Self) -> io::Result<()> {
        if !route.transitions().contains(&(*self, to)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Capture attempt phase transition refused",
            ));
        }
        *self = to;
        Ok(())
    }
}

/// The confined worker's PID namespace at a checkpoint: its init process and
/// the namespace it owns. Recovery compares a live namespace with these facts
/// before it trusts or retires anything the worker left behind.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct NamespaceFacts {
    pub(super) init_pid: u32,
    pub(super) start_ticks: u64,
    pub(super) namespace_dev: u64,
    pub(super) namespace_ino: u64,
}

impl NamespaceFacts {
    /// Facts that can name a live namespace: a real init and a namespace inode.
    pub(super) fn plausible(&self) -> bool {
        self.init_pid > 1 && self.start_ticks != 0 && self.namespace_ino != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_route_takes_only_its_own_transitions() {
        let mut phase = Phase::Prepared;
        assert!(phase.advance(Route::Medium, Phase::ScratchReady).is_err());
        assert_eq!(phase, Phase::Prepared);
        for next in [
            Phase::Launching,
            Phase::Exported,
            Phase::Verifying,
            Phase::Verified,
        ] {
            phase.advance(Route::Medium, next).unwrap();
        }
        assert!(!Route::Medium.admits(Phase::Sealing));
        assert!(!Route::Medium.admits(Phase::Cancelled));
        let mut phase = Phase::Prepared;
        assert!(phase.advance(Route::InstalledCopy, Phase::Launching).is_err());
        for next in [
            Phase::ScratchReady,
            Phase::Launching,
            Phase::Exported,
            Phase::Verifying,
            Phase::Sealing,
            Phase::ReceiverPending,
            Phase::Verified,
        ] {
            phase.advance(Route::InstalledCopy, next).unwrap();
        }
        assert!(phase.advance(Route::InstalledCopy, Phase::Cancelled).is_err());
    }
}
