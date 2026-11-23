//! Ordering of registry version strings: build metadata is identity, never update precedence, and
//! versions that do not parse as `SemVer` order numerically by field.

use std::cmp::Ordering;

pub fn compare(left: &str, right: &str) -> Ordering {
    match (semver::Version::parse(left), semver::Version::parse(right)) {
        (Ok(left), Ok(right)) => left.cmp_precedence(&right),
        _ => numeric_key(left)
            .cmp(&numeric_key(right))
            .then_with(|| left.cmp(right)),
    }
}

/// Whether `version` is a pre-release, which only the development channel installs. Versions that
/// are not `SemVer` count as releases.
pub fn is_prerelease(version: &str) -> bool {
    semver::Version::parse(version).is_ok_and(|version| !version.pre.is_empty())
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

    /// `compare` over two versions: `(row, left, right, the ordering)`. Each row was a test of its
    /// own, and a failure names its row and the two versions.
    #[test]
    fn versions_compare_by_development_sequence_and_by_field() {
        let rows = [
            (
                "development_sequence_is_numeric_and_hash_does_not_rank_updates",
                "0.1.1-dev.10+ga",
                "0.1.1-dev.9+gz",
                Ordering::Greater,
            ),
            (
                "development_sequence_is_numeric_and_hash_does_not_rank_updates",
                "0.1.1-dev.10+ga",
                "0.1.1-dev.10+gz",
                Ordering::Equal,
            ),
            (
                "development_sequence_is_numeric_and_hash_does_not_rank_updates",
                "0.1.1",
                "0.1.1-dev.999",
                Ordering::Greater,
            ),
            (
                "legacy_calver_and_opaque_ordering_is_retained",
                "2026.10",
                "2026.9",
                Ordering::Greater,
            ),
            (
                "legacy_calver_and_opaque_ordering_is_retained",
                "beta",
                "alpha",
                Ordering::Greater,
            ),
        ];
        for (row, left, right, expected) in rows {
            assert_eq!(
                compare(left, right),
                expected,
                "{row}: {left} against {right}"
            );
        }
    }
}
