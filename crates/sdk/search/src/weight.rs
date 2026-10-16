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

/// Rank of a query that only matches a title's words within a small edit
/// distance: a typo in a name the person remembers. A keyword's words match
/// this way one rank lower; other fields never do.
pub const FUZZY_RANK: i32 = 10;

/// How strongly a query matched one field; later variants are stronger.
///
/// A word is a run of letters or of digits: `report2026-final` has the
/// words `report`, `2026` and `final`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchTier {
    /// Every word of the query matches a title word within a small edit
    /// distance ([`Matching::typos`](crate::Matching::typos)).
    Typo,
    /// Every word of the query occurs in some field, though no field holds
    /// the whole query ([`Matching::spanning`](crate::Matching::spanning)).
    Spanning,
    /// The query occurs inside the field, not at a word start.
    Contains,
    /// Some later word of the field starts with the query.
    WordPrefix,
    /// The query is some later whole word (or words) of the field.
    Word,
    /// The field starts with the query.
    Prefix,
    /// The field starts with the query, which ends at a word end.
    PrefixWord,
    /// The field is the query.
    Exact,
}

/// The rank a tier earns in a field of some weight; `None` skips the tier.
/// An index ranks with [`MatchTier::rank_in`] unless given its own table.
pub type Ranks = fn(MatchTier, FieldWeight) -> Option<i32>;

impl MatchTier {
    /// The tiers found in one field, weakest first.
    pub(crate) const FIELD: [Self; 6] = [
        Self::Contains,
        Self::WordPrefix,
        Self::Word,
        Self::Prefix,
        Self::PrefixWord,
        Self::Exact,
    ];

    /// The tier's share of a full-weight field, in percent.
    #[must_use]
    pub const fn percent(self) -> i32 {
        match self {
            Self::Exact => 100,
            Self::PrefixWord => 94,
            Self::Prefix => 88,
            Self::Word => 80,
            Self::WordPrefix => 72,
            Self::Contains => 60,
            Self::Spanning => SPANNING_RANK,
            Self::Typo => FUZZY_RANK,
        }
    }

    /// The default rank of this tier in a field of `weight`. Unranked fields
    /// only match by substring, at rank 0; the loose tiers rank the same in
    /// every field.
    #[must_use]
    pub fn rank_in(self, weight: FieldWeight) -> Option<i32> {
        match self {
            Self::Spanning => Some(self.percent()),
            // A name remembered wrongly beats a keyword remembered wrongly.
            Self::Typo if weight == FieldWeight::TITLE => Some(self.percent()),
            Self::Typo => Some(self.percent().saturating_sub(1)),
            Self::Contains if weight.percent() == 0 => Some(0),
            _ if weight.percent() == 0 => None,
            _ => Some(self.percent().saturating_mul(i32::from(weight.percent())) / 100),
        }
    }
}

/// The share of [`MAX_FIELD_RANK`] a match in one field is worth, in percent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldWeight(u8);

impl FieldWeight {
    /// The name a person looks for: a title, a contact's name, a file name.
    pub const TITLE: Self = Self(100);
    /// A word the person uses for the thing without it being its name: an
    /// alias, a synonym, the product it stands in for ("youtube" for a video
    /// app), the same in another language. Ranks below the name and above
    /// identities; its words also match by typo.
    pub const KEYWORD: Self = Self(80);
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

/// The slot [`FieldWeight::KEYWORD`] is kept in.
pub const KEYWORD_SLOT: usize = 3;

/// The weights an index keeps a field for: the standard ones plus the
/// action and package identities the launcher ranks just below a title.
pub const INDEXED_WEIGHTS: [FieldWeight; 9] = [
    FieldWeight::TITLE,
    FieldWeight::new(90),
    FieldWeight::new(88),
    FieldWeight::KEYWORD,
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
        assert_eq!(FieldWeight::UNRANKED.slot(), 8);
        assert_eq!(FieldWeight::new(89).slot(), 1);
        assert_eq!(FieldWeight::KEYWORD.slot(), KEYWORD_SLOT);
        assert_eq!(FieldWeight::new(45).slot(), 4);
        assert_eq!(FieldWeight::new(200), FieldWeight::TITLE);
    }

    #[test]
    fn tiers_scale_with_weight() {
        assert_eq!(
            MatchTier::Exact.rank_in(FieldWeight::TITLE),
            Some(MAX_FIELD_RANK)
        );
        assert_eq!(MatchTier::Contains.rank_in(FieldWeight::BODY), Some(12));
        assert_eq!(MatchTier::Contains.rank_in(FieldWeight::UNRANKED), Some(0));
        assert_eq!(MatchTier::Prefix.rank_in(FieldWeight::UNRANKED), None);
    }
}
