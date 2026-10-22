//! Cards, hand ranking and pots: the rules of Texas hold'em, with no state of
//! their own.
//!
//! A card is a number below 52: `rank = card % 13` (0 is a two, 12 an ace)
//! and `suit = card / 13`. The resource stores cards as these numbers, so a
//! deck is a plain list every replica shuffles the same way from one seed.

pub const DECK_SIZE: u8 = 52;

/// The rank of `card`: 0 for a two up to 12 for an ace.
#[must_use]
pub const fn rank(card: u8) -> u8 {
    card % 13
}

/// The suit of `card`: 0 spades, 1 hearts, 2 diamonds, 3 clubs.
#[must_use]
pub const fn suit(card: u8) -> u8 {
    card / 13
}

/// Whether `card` is a heart or a diamond.
#[must_use]
pub const fn is_red(card: u8) -> bool {
    matches!(suit(card), 1 | 2)
}

/// How a rank is printed on a card face.
#[must_use]
pub const fn rank_label(card: u8) -> &'static str {
    match rank(card) {
        0 => "2",
        1 => "3",
        2 => "4",
        3 => "5",
        4 => "6",
        5 => "7",
        6 => "8",
        7 => "9",
        8 => "10",
        9 => "J",
        10 => "Q",
        11 => "K",
        _ => "A",
    }
}

/// The Fluent id naming a rank, for screen readers ("Queen").
#[must_use]
pub const fn rank_message(card: u8) -> &'static str {
    match rank(card) {
        0 => "poker-rank-2",
        1 => "poker-rank-3",
        2 => "poker-rank-4",
        3 => "poker-rank-5",
        4 => "poker-rank-6",
        5 => "poker-rank-7",
        6 => "poker-rank-8",
        7 => "poker-rank-9",
        8 => "poker-rank-10",
        9 => "poker-rank-jack",
        10 => "poker-rank-queen",
        11 => "poker-rank-king",
        _ => "poker-rank-ace",
    }
}

/// The Fluent id naming a suit ("hearts").
#[must_use]
pub const fn suit_message(card: u8) -> &'static str {
    match suit(card) {
        0 => "poker-suit-spades",
        1 => "poker-suit-hearts",
        2 => "poker-suit-diamonds",
        _ => "poker-suit-clubs",
    }
}

/// A full deck in `seed`'s order: a Fisher–Yates shuffle driven by a
/// `SplitMix64` stream, so every replica that applies the deal produces the
/// same deck.
#[must_use]
pub fn shuffled(seed: u64) -> Vec<u8> {
    let mut deck: Vec<u8> = (0..DECK_SIZE).collect();
    let mut state = seed;
    for i in (1..deck.len()).rev() {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let bound = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
        let j = usize::try_from(z.checked_rem(bound).unwrap_or(0)).unwrap_or(0);
        deck.swap(i, j);
    }
    deck
}

/// The category of a five-card hand, weakest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    HighCard,
    Pair,
    TwoPair,
    Trips,
    Straight,
    Flush,
    FullHouse,
    Quads,
    StraightFlush,
}

impl Category {
    /// The Fluent id naming the category ("Full house").
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::HighCard => "poker-category-high-card",
            Self::Pair => "poker-category-pair",
            Self::TwoPair => "poker-category-two-pair",
            Self::Trips => "poker-category-trips",
            Self::Straight => "poker-category-straight",
            Self::Flush => "poker-category-flush",
            Self::FullHouse => "poker-category-full-house",
            Self::Quads => "poker-category-quads",
            Self::StraightFlush => "poker-category-straight-flush",
        }
    }

    const fn from_index(index: u32) -> Self {
        match index {
            0 => Self::HighCard,
            1 => Self::Pair,
            2 => Self::TwoPair,
            3 => Self::Trips,
            4 => Self::Straight,
            5 => Self::Flush,
            6 => Self::FullHouse,
            7 => Self::Quads,
            _ => Self::StraightFlush,
        }
    }
}

/// The strength of a hand: higher beats lower, equal splits the pot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Strength(u32);

impl Strength {
    /// The hand's category.
    #[must_use]
    pub const fn category(self) -> Category {
        Category::from_index(self.0 >> 20)
    }
}

/// The strength of exactly five cards.
fn five(cards: [u8; 5]) -> Strength {
    let mut counts = [0_u8; 13];
    for card in cards {
        if let Some(count) = counts.get_mut(usize::from(rank(card))) {
            *count = count.saturating_add(1);
        }
    }
    let flush = cards.iter().all(|card| suit(*card) == suit(cards[0]));
    // The highest card of a straight, the wheel (A-2-3-4-5) counting as five high.
    let straight_high = (4..13_usize)
        .rev()
        .find(|high| {
            (0..5).all(|step| {
                high.checked_sub(step)
                    .and_then(|r| counts.get(r))
                    .is_some_and(|c| *c == 1)
            })
        })
        .or_else(|| {
            let wheel = [12_usize, 0, 1, 2, 3];
            wheel
                .iter()
                .all(|r| counts.get(*r).is_some_and(|c| *c == 1))
                .then_some(3)
        });
    // Ranks ordered by how many there are, then by rank: what breaks ties.
    let mut groups: Vec<(u8, u8)> = counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(r, count)| (*count, u8::try_from(r).unwrap_or(0)))
        .collect();
    groups.sort_unstable_by(|a, b| b.cmp(a));
    let shape: Vec<u8> = groups.iter().map(|(count, _)| *count).collect();
    let category = match (straight_high, flush, shape.as_slice()) {
        (Some(_), true, _) => Category::StraightFlush,
        (_, _, [4, ..]) => Category::Quads,
        (_, _, [3, 2]) => Category::FullHouse,
        (_, true, _) => Category::Flush,
        (Some(_), false, _) => Category::Straight,
        (_, _, [3, ..]) => Category::Trips,
        (_, _, [2, 2, ..]) => Category::TwoPair,
        (_, _, [2, ..]) => Category::Pair,
        _ => Category::HighCard,
    };
    let kickers: Vec<u8> = match (category, straight_high) {
        (Category::StraightFlush | Category::Straight, Some(high)) => {
            vec![u8::try_from(high).unwrap_or(0)]
        }
        _ => groups.iter().map(|(_, r)| *r).collect(),
    };
    let mut score = (category as u32) << 20;
    for (place, kicker) in kickers.iter().take(5).enumerate() {
        let shift = 16_u32.saturating_sub(u32::try_from(place).unwrap_or(0).saturating_mul(4));
        score |= u32::from(*kicker) << shift;
    }
    Strength(score)
}

/// The best five-card hand among `cards` (five to seven of them).
#[must_use]
pub fn best(cards: &[u8]) -> Option<Strength> {
    if cards.len() < 5 {
        return None;
    }
    let n = cards.len();
    let mut best: Option<Strength> = None;
    // Every way to leave out `n - 5` cards.
    for mask in 0_u32..(1 << n) {
        if mask.count_ones() != 5 {
            continue;
        }
        let mut hand = [0_u8; 5];
        let mut at = 0;
        for (i, card) in cards.iter().enumerate() {
            if mask & (1 << i) != 0
                && let Some(slot) = hand.get_mut(at)
            {
                *slot = *card;
                at = at.saturating_add(1);
            }
        }
        let strength = five(hand);
        if best.is_none_or(|b| strength > b) {
            best = Some(strength);
        }
    }
    best
}

/// One pot: how much is in it and which seats may win it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pot {
    pub amount: i64,
    /// The positions (into the hand's players) that contributed to it and
    /// have not folded.
    pub eligible: Vec<usize>,
}

/// The main pot and side pots that `committed` (each player's chips in the
/// hand) makes, given who has `folded`. A folded player's chips stay in the
/// pots they reached, but they can win none.
#[must_use]
pub fn pots(committed: &[i64], folded: &[bool]) -> Vec<Pot> {
    let mut levels: Vec<i64> = committed
        .iter()
        .zip(folded)
        .filter(|(amount, folded)| **amount > 0 && !**folded)
        .map(|(amount, _)| *amount)
        .collect();
    levels.sort_unstable();
    levels.dedup();
    let mut pots = Vec::new();
    let mut floor = 0_i64;
    for level in levels {
        let amount: i64 = committed
            .iter()
            .map(|c| (*c).min(level).saturating_sub(floor).max(0))
            .sum();
        let eligible: Vec<usize> = committed
            .iter()
            .zip(folded)
            .enumerate()
            .filter(|(_, (c, f))| **c >= level && !**f)
            .map(|(i, _)| i)
            .collect();
        if amount > 0 {
            pots.push(Pot { amount, eligible });
        }
        floor = level;
    }
    // Chips committed above every live player's level (a folded player's
    // larger bet) go to the last pot.
    let counted: i64 = pots.iter().map(|pot| pot.amount).sum();
    let total: i64 = committed.iter().sum();
    if let Some(last) = pots.last_mut() {
        last.amount = last.amount.saturating_add(total.saturating_sub(counted));
    }
    pots
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card from its rank (0 = two) and suit.
    fn card(rank: u8, suit: u8) -> u8 {
        suit.saturating_mul(13).saturating_add(rank)
    }

    #[test]
    fn categories_rank_in_order() {
        let royal = [
            card(12, 0),
            card(11, 0),
            card(10, 0),
            card(9, 0),
            card(8, 0),
        ];
        let quads = [card(5, 0), card(5, 1), card(5, 2), card(5, 3), card(1, 0)];
        let boat = [card(5, 0), card(5, 1), card(5, 2), card(1, 3), card(1, 0)];
        let flush = [card(0, 1), card(4, 1), card(7, 1), card(9, 1), card(11, 1)];
        let wheel = [card(12, 0), card(0, 1), card(1, 2), card(2, 3), card(3, 0)];
        let pair = [card(7, 0), card(7, 1), card(2, 2), card(4, 3), card(11, 0)];
        let strengths: Vec<Strength> = [royal, quads, boat, flush, wheel, pair]
            .into_iter()
            .map(five)
            .collect();
        assert!(strengths.windows(2).all(|pair| pair[0] > pair[1]));
        assert_eq!(strengths[4].category(), Category::Straight);
        assert_eq!(strengths[0].category(), Category::StraightFlush);
    }

    #[test]
    fn the_wheel_loses_to_a_six_high_straight() {
        let wheel = [card(12, 0), card(0, 1), card(1, 2), card(2, 3), card(3, 0)];
        let six = [card(4, 0), card(0, 1), card(1, 2), card(2, 3), card(3, 0)];
        assert!(five(six) > five(wheel));
    }

    #[test]
    fn kickers_break_ties_and_equal_hands_split() {
        let board = [card(12, 0), card(12, 1), card(7, 2), card(4, 3), card(2, 0)];
        let king = [card(11, 2), card(0, 3)];
        let queen = [card(10, 2), card(0, 2)];
        let a = best(&[&board[..], &king[..]].concat()).unwrap();
        let b = best(&[&board[..], &queen[..]].concat()).unwrap();
        assert!(a > b);
        let c = best(&[&board[..], &[card(11, 3), card(1, 1)][..]].concat()).unwrap();
        assert_eq!(a, c);
    }

    #[test]
    fn best_of_seven_finds_the_flush() {
        let cards = [
            card(0, 1),
            card(4, 1),
            card(7, 1),
            card(9, 1),
            card(11, 1),
            card(11, 0),
            card(11, 2),
        ];
        assert_eq!(best(&cards).unwrap().category(), Category::Flush);
    }

    #[test]
    fn a_shuffle_is_a_permutation_fixed_by_its_seed() {
        let deck = shuffled(42);
        let mut sorted = deck.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..52).collect::<Vec<u8>>());
        assert_eq!(deck, shuffled(42));
        assert_ne!(deck, shuffled(43));
    }

    #[test]
    fn side_pots_follow_all_in_levels() {
        // A is all in for 50, B and C put in 200, D folded after 20.
        let pots = pots(&[50, 200, 200, 20], &[false, false, false, true]);
        assert_eq!(pots.len(), 2);
        assert_eq!(pots[0].amount, 50 * 3 + 20);
        assert_eq!(pots[0].eligible, vec![0, 1, 2]);
        assert_eq!(pots[1].amount, 150 * 2);
        assert_eq!(pots[1].eligible, vec![1, 2]);
    }
}
