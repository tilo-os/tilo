//! Size tiers: how a body of a given size is chunked.
//!
//! | Body size        | min / avg / max        |
//! |------------------|------------------------|
//! | ≤ 4 KiB          | one chunk (inline)     |
//! | ≤ 64 KiB         | one chunk              |
//! | 64 KiB – 64 MiB  | 16 / 64 / 256 KiB      |
//! | 64 MiB – 4 GiB   | 64 / 256 KiB / 1 MiB   |
//! | > 4 GiB          | 256 KiB / 1 / 4 MiB    |
//!
//! A record keeps its tier until its size moves past a boundary by 4×, so a
//! file that hovers at a boundary keeps its dedup with earlier versions.

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

pub const INLINE_MAX: u64 = 4 * KIB;
pub const SINGLE_MAX: u64 = 64 * KIB;
const SMALL_MAX: u64 = 64 * MIB;
const MEDIUM_MAX: u64 = 4 * GIB;
/// How far past a tier boundary a record must move before it changes tier.
const TIER_HYSTERESIS: u64 = 4;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum ChunkTier {
    /// Small enough to travel inside the record operation; one chunk.
    Inline,
    /// One chunk.
    Single,
    Small,
    Medium,
    Large,
}

impl ChunkTier {
    /// The tier a body of `len` bytes gets when it has no history.
    pub const fn for_size(len: u64) -> Self {
        if len <= INLINE_MAX {
            Self::Inline
        } else if len <= SINGLE_MAX {
            Self::Single
        } else if len <= SMALL_MAX {
            Self::Small
        } else if len <= MEDIUM_MAX {
            Self::Medium
        } else {
            Self::Large
        }
    }

    /// Size range in which `self` is kept once a record has it.
    const fn sticky_range(self) -> (u64, u64) {
        let (lo, hi) = match self {
            Self::Inline => (0, INLINE_MAX),
            Self::Single => (INLINE_MAX, SINGLE_MAX),
            Self::Small => (SINGLE_MAX, SMALL_MAX),
            Self::Medium => (SMALL_MAX, MEDIUM_MAX),
            Self::Large => (MEDIUM_MAX, u64::MAX),
        };
        (lo / TIER_HYSTERESIS, hi.saturating_mul(TIER_HYSTERESIS))
    }

    /// The tier for a record that had `previous` and now has `len` bytes.
    ///
    /// Unchunked bodies always follow the size; the hysteresis applies
    /// between chunked tiers, where a change of chunk targets would lose all
    /// dedup with the previous version.
    pub fn sticky(previous: Option<Self>, len: u64) -> Self {
        let fresh = Self::for_size(len);
        match previous {
            Some(prev) if prev >= Self::Small && fresh >= Self::Small => {
                let (lo, hi) = prev.sticky_range();
                if len > lo && len <= hi { prev } else { fresh }
            }
            _ => fresh,
        }
    }

    /// `FastCDC` `(min, avg, max)` chunk sizes, or `None` for tiers that are
    /// one chunk.
    pub const fn cdc_params(self) -> Option<(u32, u32, u32)> {
        match self {
            Self::Inline | Self::Single => None,
            Self::Small => Some((16 * 1024, 64 * 1024, 256 * 1024)),
            Self::Medium => Some((64 * 1024, 256 * 1024, 1024 * 1024)),
            Self::Large => Some((256 * 1024, 1024 * 1024, 4 * 1024 * 1024)),
        }
    }

    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Inline => 0,
            Self::Single => 1,
            Self::Small => 2,
            Self::Medium => 3,
            Self::Large => 4,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Inline),
            1 => Some(Self::Single),
            2 => Some(Self::Small),
            3 => Some(Self::Medium),
            4 => Some(Self::Large),
            _ => None,
        }
    }

    /// The largest chunk this tier produces, or `None` when the body is one
    /// chunk of any size up to [`SINGLE_MAX`].
    pub const fn max_chunk(self) -> Option<u32> {
        match self.cdc_params() {
            Some((_, _, max)) => Some(max),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_follow_the_size_table() {
        assert_eq!(ChunkTier::for_size(0), ChunkTier::Inline);
        assert_eq!(ChunkTier::for_size(4096), ChunkTier::Inline);
        assert_eq!(ChunkTier::for_size(4097), ChunkTier::Single);
        assert_eq!(ChunkTier::for_size(64 * KIB), ChunkTier::Single);
        assert_eq!(ChunkTier::for_size(64 * KIB + 1), ChunkTier::Small);
        assert_eq!(ChunkTier::for_size(64 * MIB + 1), ChunkTier::Medium);
        assert_eq!(ChunkTier::for_size(5 * GIB), ChunkTier::Large);
    }

    #[test]
    fn chunked_tiers_stick_until_the_size_moves_four_times_past_the_boundary() {
        let s = Some(ChunkTier::Small);
        assert_eq!(ChunkTier::sticky(s, 65 * MIB), ChunkTier::Small);
        assert_eq!(ChunkTier::sticky(s, 256 * MIB), ChunkTier::Small);
        assert_eq!(ChunkTier::sticky(s, 257 * MIB), ChunkTier::Medium);
        let m = Some(ChunkTier::Medium);
        assert_eq!(ChunkTier::sticky(m, 63 * MIB), ChunkTier::Medium);
        assert_eq!(ChunkTier::sticky(m, 15 * MIB), ChunkTier::Small);
        // Unchunked tiers always follow the size.
        assert_eq!(ChunkTier::sticky(s, 1000), ChunkTier::Inline);
    }

    #[test]
    fn codes_round_trip() {
        for tier in [
            ChunkTier::Inline,
            ChunkTier::Single,
            ChunkTier::Small,
            ChunkTier::Medium,
            ChunkTier::Large,
        ] {
            assert_eq!(ChunkTier::from_code(tier.code()), Some(tier));
        }
        assert_eq!(ChunkTier::from_code(9), None);
    }
}
