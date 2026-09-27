//! Field weights and match tiers: the ranking every index shares, so "an
//! exact title beats a title prefix beats a body mention" means the same
//! thing on every search surface.
//!
//! A hit ranks by the best of its weighted fields: the [`MatchTier`] of the
//! query in that field, scaled by the field's [`FieldWeight`], on a
//! `0..=`[`MAX_FIELD_RANK`] scale.

/// Top of the rank scale: an exact match in a full-weight field.
pub const MAX_FIELD_RANK: i32 = 100;

/// Rank of a multi-word query whose every word occurs in some field, though
/// no field holds the whole query: below a substring match in any standard
/// field (the lightest, [`FieldWeight::BODY`], ranks an infix at 12).
pub const SPANNING_RANK: i32 = 11;

/// Rank of a query that only matches a full-weight field's words within a
/// small edit distance: a typo in a name the person remembers. Lighter
/// fields never match this way.
pub const FUZZY_RANK: i32 = 10;

/// How strongly a query matched one field; later variants are stronger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchTier {
    /// The query occurs inside the field, not at a word start.
    Contains,
    /// Some later word of the field starts with the query.
    WordPrefix,
    /// The field starts with the query.
    Prefix,
    /// The field is the query.
    Exact,
}

impl MatchTier {
    pub(crate) const ALL: [Self; 4] = [Self::Contains, Self::WordPrefix, Self::Prefix, Self::Exact];

    /// The tier's share of a full-weight field, in percent.
    #[must_use]
    pub const fn percent(self) -> i32 {
        match self {
            Self::Exact => 100,
            Self::Prefix => 88,
            Self::WordPrefix => 72,
            Self::Contains => 60,
        }
    }

    /// The rank this tier earns in a field of `weight`.
    #[must_use]
    pub fn rank_in(self, weight: FieldWeight) -> i32 {
        self.percent().saturating_mul(i32::from(weight.percent())) / 100
    }
}

/// The share of [`MAX_FIELD_RANK`] a match in one field is worth, in percent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldWeight(u8);

impl FieldWeight {
    /// The name a person looks for: a title, a contact's name, a file name.
    pub const TITLE: Self = Self(100);
    /// A stable identity the person may type verbatim: an id, a path, a key.
    pub const IDENTITY: Self = Self(50);
    /// A secondary line shown with the hit: an author, a sender, a folder.
    pub const SUBTITLE: Self = Self(40);
    /// Other short fields: labels, tags, addresses.
    pub const METADATA: Self = Self(30);
    /// Long prose: a message body, a chapter, a note.
    pub const BODY: Self = Self(20);
    /// Searchable, but never reorders hits.
    pub const UNRANKED: Self = Self(0);

    /// A custom weight; values above 100 are clamped. An index keeps one
    /// field per weight in [`INDEXED_WEIGHTS`]; a custom weight ranks as the
    /// nearest of them.
    #[must_use]
    pub const fn new(percent: u8) -> Self {
        if percent > 100 {
            Self(100)
        } else {
            Self(percent)
        }
    }

    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }

    /// The indexed field slot this weight is stored in.
    pub(crate) fn slot(self) -> usize {
        let mut best = 0usize;
        let mut best_distance = u8::MAX;
        for (slot, weight) in INDEXED_WEIGHTS.iter().enumerate() {
            let distance = weight.0.abs_diff(self.0);
            if distance < best_distance {
                best = slot;
                best_distance = distance;
            }
        }
        best
    }
}

/// The weights an index keeps a field for: the standard ones plus the
/// action and package identities the launcher ranks just below a title.
pub const INDEXED_WEIGHTS: [FieldWeight; 8] = [
    FieldWeight::TITLE,
    FieldWeight::new(90),
    FieldWeight::new(88),
    FieldWeight::IDENTITY,
    FieldWeight::SUBTITLE,
    FieldWeight::METADATA,
    FieldWeight::BODY,
    FieldWeight::UNRANKED,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_find_their_slot() {
        assert_eq!(FieldWeight::TITLE.slot(), 0);
        assert_eq!(FieldWeight::UNRANKED.slot(), 7);
        assert_eq!(FieldWeight::new(89).slot(), 1);
        assert_eq!(FieldWeight::new(45).slot(), 3);
        assert_eq!(FieldWeight::new(200), FieldWeight::TITLE);
    }

    #[test]
    fn tiers_scale_with_weight() {
        assert_eq!(MatchTier::Exact.rank_in(FieldWeight::TITLE), MAX_FIELD_RANK);
        assert_eq!(MatchTier::Contains.rank_in(FieldWeight::BODY), 12);
        assert_eq!(MatchTier::Prefix.rank_in(FieldWeight::UNRANKED), 0);
    }
}
