//! Fractional order keys: a key strictly between any two distinct keys always
//! exists, so an ordered collection survives concurrent inserts and moves
//! without renumbering its neighbours.
//!
//! Keys are base-36 fractions written with `0-9a-z`. They never end in `0`,
//! which makes plain string order the same as fraction order, so a key can be
//! stored as an ordinary string scalar and compared anywhere. [`bytes`] is
//! the same idea in base 256, for documents that store keys as bytes.

pub mod bytes;

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const BASE: u8 = 36;

fn digit(key: &[u8], index: usize) -> u8 {
    key.get(index)
        .and_then(|c| DIGITS.iter().position(|d| d == c))
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(0)
}

fn char_of(value: u8) -> char {
    DIGITS
        .get(usize::from(value))
        .map_or('0', |d| char::from(*d))
}

/// A key strictly after `low` and strictly before `high`. `None` bounds are
/// the open ends of the collection. When `low >= high` (two replicas minted
/// the same key concurrently), the result is after `low`; callers break the
/// remaining tie by item identity.
#[must_use]
pub fn between(low: Option<&str>, high: Option<&str>) -> String {
    let low = low.unwrap_or("").as_bytes();
    let mut high = high.map(str::as_bytes).filter(|high| *high > low);
    let mut out = String::new();
    let mut index = 0usize;
    loop {
        let lo = digit(low, index);
        let hi = high.map_or(BASE, |high| digit(high, index));
        if lo == hi {
            out.push(char_of(lo));
        } else if hi.saturating_sub(lo) > 1 {
            out.push(char_of(lo.saturating_add(hi) / 2));
            return out;
        } else {
            // Adjacent digits: keep `lo` and look for room below the open end.
            out.push(char_of(lo));
            high = None;
        }
        index = index.saturating_add(1);
    }
}

/// `count` keys in ascending order, all strictly between `low` and `high`.
#[must_use]
pub fn sequence(low: Option<&str>, high: Option<&str>, count: usize) -> Vec<String> {
    let mut keys = Vec::with_capacity(count);
    let mut previous = low.map(str::to_owned);
    for _ in 0..count {
        let key = between(previous.as_deref(), high);
        previous = Some(key.clone());
        keys.push(key);
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_order_keys_stay_strictly_ordered() {
        // phase: between_is_strictly_ordered_and_never_ends_in_zero
        {
            let cases = [
                (None, None),
                (Some("a"), None),
                (None, Some("a")),
                (Some("a"), Some("b")),
                (Some("a"), Some("a1")),
                (Some("zz"), None),
                (Some("0001"), Some("0002")),
            ];
            for (low, high) in cases {
                let key = between(low, high);
                assert!(!key.ends_with('0'), "{key}");
                if let Some(low) = low {
                    assert!(key.as_str() > low, "{low:?} < {key}");
                }
                if let Some(high) = high {
                    assert!(key.as_str() < high, "{key} < {high:?}");
                }
            }
        }
        // phase: repeated_insertion_at_one_point_stays_ordered
        {
            let mut low = between(None, None);
            let high = between(Some(&low), None);
            for _ in 0..200 {
                let key = between(Some(&low), Some(&high));
                assert!(key > low && key < high);
                low = key;
            }
            let keys = sequence(Some("a"), Some("b"), 50);
            assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}
