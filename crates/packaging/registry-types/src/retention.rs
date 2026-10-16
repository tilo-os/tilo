//! How many versions an entry keeps.
//!
//! Versions are grouped by target (platform and architecture) and channel
//! (release or pre-release). Publishers keep the newest `releases` or
//! `prereleases` of each group and drop the rest when they sign the next
//! entry; the registry then stops referencing the dropped artifacts and
//! collects their storage. Every group keeps at least
//! [`MIN_RETAINED_PER_CHANNEL`], and the registry refuses an entry that drops
//! a version with fewer newer ones of its group left, so retention never
//! removes a target's current version or its recent rollback targets.

use std::cmp::Ordering;

use crate::RegistryVersion;

/// Newer versions of the same target and channel that must remain before a
/// version may leave an entry.
pub const MIN_RETAINED_PER_CHANNEL: usize = 4;

/// Versions kept per target and channel unless the publisher asks otherwise.
pub const DEFAULT_RETAINED_PER_CHANNEL: usize = 20;

/// The newest versions an entry keeps per target and channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub releases: usize,
    pub prereleases: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            releases: DEFAULT_RETAINED_PER_CHANNEL,
            prereleases: DEFAULT_RETAINED_PER_CHANNEL,
        }
    }
}

impl RetentionPolicy {
    /// Versions kept in one group, never below [`MIN_RETAINED_PER_CHANNEL`].
    #[must_use]
    pub fn limit(&self, prerelease: bool) -> usize {
        let configured = if prerelease {
            self.prereleases
        } else {
            self.releases
        };
        configured.max(MIN_RETAINED_PER_CHANNEL)
    }

    /// `versions` without the ones this policy drops, in their original order.
    #[must_use]
    pub fn retain(&self, versions: &[RegistryVersion]) -> Vec<RegistryVersion> {
        versions
            .iter()
            .filter(|version| {
                newer_in_group(version, versions) < self.limit(is_prerelease(version))
            })
            .cloned()
            .collect()
    }
}

/// Whether `old` may leave an entry whose versions become `remaining`: at
/// least [`MIN_RETAINED_PER_CHANNEL`] newer versions of its target and
/// channel stay.
#[must_use]
pub fn may_drop(old: &RegistryVersion, remaining: &[RegistryVersion]) -> bool {
    newer_in_group(old, remaining) >= MIN_RETAINED_PER_CHANNEL
}

/// Same version group (target, or bot) and channel.
#[must_use]
pub fn same_group(left: &RegistryVersion, right: &RegistryVersion) -> bool {
    left.group() == right.group() && is_prerelease(left) == is_prerelease(right)
}

fn is_prerelease(version: &RegistryVersion) -> bool {
    crate::version::is_prerelease(&version.version)
}

fn newer_in_group(version: &RegistryVersion, versions: &[RegistryVersion]) -> usize {
    versions
        .iter()
        .filter(|other| {
            same_group(other, version)
                && crate::version::compare(&other.version, &version.version) == Ordering::Greater
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RegistryTarget, TappArtifact, VersionArtifact};

    fn version(value: &str, architecture: &str) -> RegistryVersion {
        RegistryVersion {
            version: value.into(),
            released_at_unix: 0,
            artifact: VersionArtifact::Tapp(TappArtifact {
                tapp_url: String::new(),
                tapp_root_hash_hex: String::new(),
                tapp_size_bytes: 0,
                tapp_blake3_hex: String::new(),
                target: RegistryTarget {
                    platform: crate::TILO_OS_PLATFORM.into(),
                    architecture: architecture.into(),
                    isa: String::new(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }
    }

    fn names(versions: &[RegistryVersion]) -> Vec<String> {
        versions
            .iter()
            .map(|v| format!("{}/{}", v.version, v.tapp().unwrap().target.architecture))
            .collect()
    }

    #[test]
    fn keeps_the_newest_of_each_target_and_channel() {
        let mut versions: Vec<_> = (1..=10)
            .map(|n| version(&format!("1.0.{n}"), "x86_64"))
            .collect();
        versions.extend((1..=10).map(|n| version(&format!("1.1.0-dev.{n}"), "x86_64")));
        versions.push(version("0.9.0", "aarch64"));
        let policy = RetentionPolicy {
            releases: 5,
            prereleases: 6,
        };
        let kept = policy.retain(&versions);
        let expected: Vec<String> = (6..=10)
            .map(|n| format!("1.0.{n}/x86_64"))
            .chain((5..=10).map(|n| format!("1.1.0-dev.{n}/x86_64")))
            .chain(["0.9.0/aarch64".to_owned()])
            .collect();
        assert_eq!(names(&kept), expected);
        // Every dropped version has enough newer ones left to be dropped.
        for dropped in versions.iter().filter(|v| !kept.contains(v)) {
            assert!(may_drop(dropped, &kept));
        }
        // Applying the policy again changes nothing.
        assert_eq!(policy.retain(&kept), kept);
    }

    #[test]
    fn the_floor_protects_recent_versions() {
        let versions: Vec<_> = (1..=10)
            .map(|n| version(&format!("1.0.{n}"), "x86_64"))
            .collect();
        let policy = RetentionPolicy {
            releases: 0,
            prereleases: 0,
        };
        assert_eq!(policy.retain(&versions).len(), MIN_RETAINED_PER_CHANNEL);
        let newest = &versions[6..];
        assert!(may_drop(&versions[5], newest));
        assert!(!may_drop(&versions[6], &versions[7..]));
        // A newer pre-release does not count toward a release's group.
        let mut mixed: Vec<_> = (1..=4)
            .map(|n| version(&format!("2.0.0-dev.{n}"), "x86_64"))
            .collect();
        assert!(!may_drop(&versions[0], &mixed));
        mixed.extend(versions[6..].iter().cloned());
        assert!(may_drop(&versions[0], &mixed));
    }
}
