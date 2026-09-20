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
        self == Self::Development
            || semver::Version::parse(version).map_or(true, |v| v.pre.is_empty())
    }
}

pub fn compare(left: &str, right: &str) -> Ordering {
    match (semver::Version::parse(left), semver::Version::parse(right)) {
        (Ok(left), Ok(right)) => left.cmp_precedence(&right),
        _ => numeric_key(left)
            .cmp(&numeric_key(right))
            .then_with(|| left.cmp(right)),
    }
}

fn numeric_key(version: &str) -> Option<Vec<u64>> {
    if let Ok(version) = semver::Version::parse(version) {
        return Some(vec![version.major, version.minor, version.patch]);
    }
    version.split('.').map(|part| part.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_sequence_is_numeric_and_hash_does_not_rank_updates() {
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

    #[test]
    fn stable_channel_keeps_explicit_release_visible() {
        assert!(!ReleaseChannel::Stable.accepts("0.1.1-dev.10+gabc"));
        assert!(ReleaseChannel::Stable.accepts("0.1.0"));
        assert!(ReleaseChannel::Development.accepts("0.1.1-dev.10+gabc"));
        assert!(ReleaseChannel::Stable.accepts("2026.09"));
    }

    #[test]
    fn legacy_calver_and_opaque_ordering_is_retained() {
        assert_eq!(compare("2026.10", "2026.9"), Ordering::Greater);
        assert_eq!(compare("beta", "alpha"), Ordering::Greater);
    }
}
