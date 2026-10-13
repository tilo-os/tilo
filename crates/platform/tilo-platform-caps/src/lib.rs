//! What every platform crate's capabilities share: a [`Feature`] trait that
//! gives each feature its [`Availability`] on Linux, macOS and the web, the
//! column for the running target, and the [`Unsupported`] error for a
//! feature this target cannot provide.

use std::fmt;

/// Whether a feature can be used on a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    /// Usable after a user prompt (browser permission, macOS TCC).
    Prompt,
    /// Never on this target; the reason is user-facing.
    Unsupported(&'static str),
}

/// This target's column of [`Feature::targets`]. Every wasm target reports
/// the web column; native targets other than macOS report the Linux column.
pub const COLUMN: usize = if cfg!(target_family = "wasm") {
    2
} else if cfg!(target_os = "macos") {
    1
} else {
    0
};

/// One capability of a platform crate.
pub trait Feature: Copy {
    /// The feature's availability on Linux, macOS and the web, in that order.
    fn targets(self) -> [Availability; 3];

    /// Whether the feature can be used on this target. A crate whose build
    /// can leave a backend out overrides this to say so.
    fn availability(self) -> Availability {
        self.targets()[COLUMN]
    }
}

/// Whether `feature` can be used on this target.
#[must_use]
pub fn availability<F: Feature>(feature: F) -> Availability {
    feature.availability()
}

/// Whether `feature` can be used on this target without a prompt.
#[must_use]
pub fn available<F: Feature>(feature: F) -> bool {
    feature.availability() == Availability::Available
}

/// A feature this target, or this particular host, cannot provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported<F> {
    pub feature: F,
    /// User-facing.
    pub reason: &'static str,
}

impl<F: Feature> Unsupported<F> {
    /// The error for `feature` on this target.
    #[must_use]
    pub fn of(feature: F) -> Self {
        let reason = match feature.availability() {
            Availability::Unsupported(reason) => reason,
            Availability::Available | Availability::Prompt => "This host cannot do this.",
        };
        Self { feature, reason }
    }
}

impl<F> fmt::Display for Unsupported<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl<F: fmt::Debug> std::error::Error for Unsupported<F> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct WebOnlyMissing;

    impl Feature for WebOnlyMissing {
        fn targets(self) -> [Availability; 3] {
            [
                Availability::Available,
                Availability::Prompt,
                Availability::Unsupported("web"),
            ]
        }
    }

    #[test]
    fn a_feature_reads_this_targets_column() {
        assert_eq!(
            availability(WebOnlyMissing),
            WebOnlyMissing.targets()[COLUMN]
        );
    }

    #[test]
    fn unsupported_carries_the_columns_reason() {
        let reason = Unsupported::of(WebOnlyMissing).reason;
        if COLUMN == 2 {
            assert_eq!(reason, "web");
        } else {
            assert_eq!(reason, "This host cannot do this.");
        }
    }
}
