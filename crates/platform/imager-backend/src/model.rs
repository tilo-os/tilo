//! Portable readiness facts shared by native adapters and the frontend.

use crate::plan::Basis;
use serde::{Deserialize, Serialize};

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

    /// Add `blocker` once.
    pub fn block(&mut self, blocker: Blocker) {
        if !self.blockers.contains(&blocker) {
            self.blockers.push(blocker);
        }
    }

    /// Facts for a host that cannot be probed at all.
    pub fn unsupported(blocker: Blocker) -> Self {
        Self {
            basis: None,
            bitlocker_protected: false,
            blockers: vec![blocker],
        }
    }
}
