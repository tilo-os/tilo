//! Host readiness: facts plus blockers with stable IDs. Unprivileged where
//! the platform allows; the helper re-probes before mutating.

use serde::{Deserialize, Serialize};

use super::plan::Basis;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    NotUefi,
    SecureBootOn,
    BitlockerLocked,
    NotGpt,
    DiskUnsupported,
    Hibernation,
    InsufficientSpace,
    OnBattery,
    PendingOperation,
    ArchMismatch,
}

impl Blocker {
    /// Blockers the person can clear from Imager (a remedy action exists).
    pub fn has_remedy(self) -> bool {
        matches!(
            self,
            Self::SecureBootOn | Self::Hibernation | Self::OnBattery | Self::PendingOperation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    /// `None` when the disk could not be read at all (then `blockers` says why).
    pub basis: Option<Basis>,
    pub bitlocker_protected: bool,
    pub blockers: Vec<Blocker>,
}

impl HostFacts {
    /// Full Tilo may be offered: nothing blocks that Imager cannot help clear.
    pub fn offers_full_os(&self) -> bool {
        self.basis.is_some() && self.blockers.iter().all(|b| b.has_remedy())
    }
}

/// Probe this host. Never mutates; safe to call from the UI thread's worker.
pub fn probe() -> HostFacts {
    HostFacts {
        basis: None,
        bitlocker_protected: false,
        blockers: vec![Blocker::DiskUnsupported],
    }
}
