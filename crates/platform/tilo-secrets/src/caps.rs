//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::Availability;

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Storing, reading and deleting an app's secrets.
    Store,
    /// Secrets held by the operating system's keychain rather than wrapped
    /// by the browser ([`Protection::OsKeychain`](crate::Protection::OsKeychain)).
    OsKeychain,
    /// The `*_blocking` methods.
    Blocking,
}

pub(crate) const WEB_KEYCHAIN: &str =
    "Browsers have no keychain; Tilo encrypts secrets with a key the browser keeps.";
pub(crate) const WEB_BLOCKING: &str = "The browser's storage answers asynchronously.";

/// Every feature with its availability on Linux (and Windows), macOS and the
/// web, in that order. A Linux session without a Secret Service provides less; the macOS
/// Keychain may ask the user to allow access.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (Feature::Store, [Available, Available, Available]),
        (
            Feature::OsKeychain,
            [Available, Available, Unsupported(WEB_KEYCHAIN)],
        ),
        (
            Feature::Blocking,
            [Available, Available, Unsupported(WEB_BLOCKING)],
        ),
    ]
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    tilo_platform_caps::availability(CAPABILITIES, feature)
}

/// The error for `feature` on this target.
#[cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(dead_code, reason = "only the web lacks a feature")
)]
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    Unsupported::of(CAPABILITIES, feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 3] = [Feature::Store, Feature::OsKeychain, Feature::Blocking];

    #[test]
    fn every_feature_is_listed_once() {
        for feature in ALL {
            let rows = CAPABILITIES.iter().filter(|(listed, _)| *listed == feature);
            assert_eq!(rows.count(), 1, "{feature:?}");
        }
        assert_eq!(CAPABILITIES.len(), ALL.len());
    }

    #[test]
    fn this_target_reads_its_own_column() {
        assert_eq!(availability(Feature::Store), Availability::Available);
        let web = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::Blocking)
            .map(|(_, targets)| targets[2]);
        assert_eq!(web, Some(Availability::Unsupported(WEB_BLOCKING)));
    }
}
