//! The shape every platform crate's capability table shares: each feature
//! with its [`Availability`] on Linux, macOS and the web, the column for
//! the running target, and the [`Unsupported`] error for a feature this
//! target cannot provide.

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

/// Every feature `F` of one crate with its availability on Linux, macOS and
/// the web, in that order.
pub type Table<F> = [(F, [Availability; 3])];

/// This target's column of a [`Table`]. Every wasm target reports the web
/// column; native targets other than macOS report the Linux column.
pub const COLUMN: usize = if cfg!(target_family = "wasm") {
    2
} else if cfg!(target_os = "macos") {
    1
} else {
    0
};

/// Whether `feature` can be used on this target, by `table`.
#[must_use]
pub fn availability<F: Copy + PartialEq>(table: &Table<F>, feature: F) -> Availability {
    table
        .iter()
        .find(|(listed, _)| *listed == feature)
        .and_then(|(_, targets)| targets.get(COLUMN).copied())
        .unwrap_or(Availability::Unsupported(
            "This build does not know the feature.",
        ))
}

/// A feature this target, or this particular host, cannot provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported<F> {
    pub feature: F,
    /// User-facing.
    pub reason: &'static str,
}

impl<F: Copy + PartialEq> Unsupported<F> {
    /// The error for `feature` on this target, by `table`.
    #[must_use]
    pub fn of(table: &Table<F>, feature: F) -> Self {
        let reason = match availability(table, feature) {
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
    enum Feature {
        Listed,
        Missing,
    }

    const TABLE: &Table<Feature> = &[(
        Feature::Listed,
        [
            Availability::Available,
            Availability::Prompt,
            Availability::Unsupported("web"),
        ],
    )];

    #[test]
    fn a_listed_feature_reads_this_targets_column() {
        assert_eq!(availability(TABLE, Feature::Listed), TABLE[0].1[COLUMN]);
    }

    #[test]
    fn an_unlisted_feature_is_unsupported() {
        assert!(matches!(
            availability(TABLE, Feature::Missing),
            Availability::Unsupported(_)
        ));
        assert_eq!(
            Unsupported::of(TABLE, Feature::Missing).feature,
            Feature::Missing
        );
    }
}
