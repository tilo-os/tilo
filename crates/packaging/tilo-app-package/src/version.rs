//! Shared ordering for signed registry versions. Build metadata is identity,
//! never update precedence. Numeric `CalVer` and legacy opaque versions retain
//! their historical ordering when neither input parses as `SemVer`.
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseChannel {
    #[default]
    Stable,
    Development,
}

impl ReleaseChannel {
    pub fn accepts(self, version: &str) -> bool {
        self == Self::Development || !registry_types::version::is_prerelease(version)
    }
}

pub fn compare(left: &str, right: &str) -> Ordering {
    registry_types::version::compare(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering_ranks_updates_per_channel() {
        // phase: development_sequence_is_numeric_and_hash_does_not_rank_updates
        {
            assert_eq!(
                compare("0.1.1-dev.10+ga", "0.1.1-dev.9+gz"),
                Ordering::Greater
            );
            assert_eq!(
                compare("0.1.1-dev.10+ga", "0.1.1-dev.10+gz"),
                Ordering::Equal
            );
            assert_eq!(compare("0.1.1", "0.1.1-dev.999"), Ordering::Greater);
        }
        // phase: stable_channel_keeps_explicit_release_visible
        {
            assert!(!ReleaseChannel::Stable.accepts("0.1.1-dev.10+gabc"));
            assert!(ReleaseChannel::Stable.accepts("0.1.0"));
            assert!(ReleaseChannel::Development.accepts("0.1.1-dev.10+gabc"));
            assert!(ReleaseChannel::Stable.accepts("2026.09"));
        }
        // phase: legacy_calver_and_opaque_ordering_is_retained
        {
            assert_eq!(compare("2026.10", "2026.9"), Ordering::Greater);
            assert_eq!(compare("beta", "alpha"), Ordering::Greater);
        }
    }
}
