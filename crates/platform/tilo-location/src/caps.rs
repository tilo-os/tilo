//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::Availability;

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// One fix: [`request_location`](crate::request_location).
    Locate,
}

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. Every target asks the user first; macOS asks only a bundled app
/// that says why.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::Prompt;
    &[(Feature::Locate, [Prompt, Prompt, Prompt])]
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    tilo_platform_caps::availability(CAPABILITIES, feature)
}

/// The error for `feature` on this target.
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    Unsupported::of(CAPABILITIES, feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_is_listed_once() {
        assert_eq!(CAPABILITIES.len(), 1);
        assert_eq!(CAPABILITIES[0].0, Feature::Locate);
    }
}
