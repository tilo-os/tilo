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
///
/// Toward an open end the key steps rather than halves the gap (see
/// [`after`] and [`before`]), so a list appended or prepended to one item
/// at a time keeps keys a few characters long instead of growing one
/// character every few items.
#[must_use]
pub fn between(low: Option<&str>, high: Option<&str>) -> String {
    let open_high = high.is_none_or(|high| low.is_some_and(|low| high <= low));
    match (low, high) {
        (Some(low), _) if open_high && is_key(low) => return after(low),
        (None, Some(high)) if is_key(high) => return before(high),
        _ => {}
    }
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

const TOP: u8 = BASE - 1;

fn spell(prefix: u8, run: usize, body: &[u8]) -> String {
    std::iter::repeat_n(prefix, run)
        .chain(body.iter().copied())
        .map(char_of)
        .collect()
}

/// The next key after `low` with no upper bound.
///
/// Halving the gap to the end, as [`between`] does between two keys, adds
/// a character every few appends: 50,000 appends made keys 8,000
/// characters long, 208 MB of keys in all (TIL-758). Instead a key is read
/// as a run of `z`s and a body after it, and the next key adds one to the
/// body. When the body is used up (its first digit would become `z`), the
/// run and the body both grow by one: `y` → `z01`, `zyz` → `zz001`. Each
/// step multiplies the room by 36, so `n` appends need keys of about
/// `2·log36(n)` characters. A key minted the old way keeps its length.
fn after(low: &str) -> String {
    let digits: Vec<u8> = (0..low.len())
        .map(|index| digit(low.as_bytes(), index))
        .collect();
    let run = digits.iter().take_while(|&&value| value == TOP).count();
    let Some(body) = digits.get(run..).filter(|body| !body.is_empty()) else {
        // All `z`s: anything longer is after it.
        return spell(TOP, run, &[1]);
    };
    let mut body = body.to_vec();
    loop {
        // Add one to the body, carrying.
        let mut carry = true;
        for value in body.iter_mut().rev() {
            if !carry {
                break;
            }
            *value = if *value == TOP {
                0
            } else {
                carry = false;
                value.saturating_add(1)
            };
        }
        if carry || body.first() == Some(&TOP) {
            // This run's bodies are used up: one more `z`, one more digit.
            let mut next = vec![0; body.len()];
            next.push(1);
            return spell(TOP, run.saturating_add(1), &next);
        }
        // A key never ends in `0`; the next body does not either.
        if body.last() != Some(&0) {
            return spell(TOP, run, &body);
        }
    }
}

/// The next key before `high` with no lower bound: [`after`] mirrored, with
/// a run of `0`s and a body that counts down: `1` → `0zz`, `01z` → `00zzz`.
fn before(high: &str) -> String {
    let digits: Vec<u8> = (0..high.len())
        .map(|index| digit(high.as_bytes(), index))
        .collect();
    let run = digits.iter().take_while(|&&value| value == 0).count();
    let mut body = digits.get(run..).unwrap_or_default().to_vec();
    loop {
        // Take one from the body, borrowing.
        let mut borrow = true;
        for value in body.iter_mut().rev() {
            if !borrow {
                break;
            }
            *value = if *value == 0 {
                TOP
            } else {
                borrow = false;
                value.saturating_sub(1)
            };
        }
        if borrow || body.first().is_none_or(|&first| first == 0) {
            // This run's bodies are used up: one more `0`, one more digit.
            let next = vec![TOP; body.len().saturating_add(1)];
            return spell(0, run.saturating_add(1), &next);
        }
        if body.last() != Some(&0) {
            return spell(0, run, &body);
        }
    }
}

/// True when `key` is a key [`between`] could have made: non-empty, only
/// `0-9a-z`, and not ending in `0`. Only such keys compare as fractions.
#[must_use]
pub fn is_key(key: &str) -> bool {
    !key.is_empty() && !key.ends_with('0') && key.bytes().all(|c| DIGITS.contains(&c))
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
                assert!(is_key(&key), "{key}");
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
        // phase: only_between_shaped_keys_are_keys
        {
            for bad in ["", "a0", "A", "a-b", " a"] {
                assert!(!is_key(bad), "{bad:?}");
            }
            assert!(is_key("0001") && is_key("i") && is_key("zz"));
        }
    }

    #[test]
    fn keys_toward_an_open_end_stay_short() {
        // phase: appends_step_and_grow_logarithmically
        {
            assert_eq!(between(Some("i"), None), "j");
            assert_eq!(between(Some("y"), None), "z01");
            assert_eq!(between(Some("z0z"), None), "z11");
            assert_eq!(between(Some("zyz"), None), "zz001");
            assert_eq!(between(Some("zz"), None), "zz1");
            let keys = sequence(None, None, 50_000);
            assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(keys.iter().all(|key| is_key(key)));
            let longest = keys.iter().map(String::len).max().unwrap_or_default();
            assert!(longest <= 7, "{longest}");
        }
        // phase: prepends_mirror_appends
        {
            assert_eq!(between(None, Some("i")), "h");
            assert_eq!(between(None, Some("1")), "0zz");
            assert_eq!(between(None, Some("i1")), "hz");
            let mut high = between(None, None);
            for _ in 0..50_000 {
                let key = between(None, Some(&high));
                assert!(is_key(&key) && key < high, "{key} < {high}");
                high = key;
            }
            assert!(high.len() <= 7, "{high}");
        }
        // phase: a_key_minted_by_halving_keeps_its_length
        {
            let old = format!("{}r", "z".repeat(500));
            let next = between(Some(&old), None);
            assert!(next > old && next.len() == old.len() && is_key(&next));
            // A bound at or below `low` is no bound, as before.
            assert_eq!(between(Some("k"), Some("c")), "l");
        }
    }
}
