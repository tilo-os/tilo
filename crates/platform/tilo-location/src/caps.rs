//! What this crate provides on each target, as data an app can query.

use std::fmt;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// One fix: [`request_location`](crate::request_location).
    Locate,
}

/// Whether a [`Feature`] can be used on a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    /// Usable after a user prompt (browser permission, macOS TCC).
    Prompt,
    /// Never on this target; the reason is user-facing.
    Unsupported(&'static str),
}

/// A feature this target, or this particular host, cannot provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported {
    pub feature: Feature,
    /// User-facing.
    pub reason: &'static str,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for Unsupported {}

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. Every target asks the user first; macOS asks only a bundled app
/// that says why.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::Prompt;
    &[(Feature::Locate, [Prompt, Prompt, Prompt])]
};

/// This target's column of [`CAPABILITIES`]. Native targets other than
/// macOS report the Linux column.
const COLUMN: usize = if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
    2
} else if cfg!(target_os = "macos") {
    1
} else {
    0
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    CAPABILITIES
        .iter()
        .find(|(listed, _)| *listed == feature)
        .and_then(|(_, targets)| targets.get(COLUMN).copied())
        .unwrap_or(Availability::Unsupported(
            "This build does not know the feature.",
        ))
}

/// The error for `feature` on this target.
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    let reason = match availability(feature) {
        Availability::Unsupported(reason) => reason,
        Availability::Available | Availability::Prompt => "This host cannot do this.",
    };
    Unsupported { feature, reason }
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
