//! Fractional order keys as bytes: base-256 fractions where a midpoint
//! always exists between two distinct keys, compared as fractions (trailing
//! zeros are insignificant). The compact form rich-text documents store per
//! paragraph; see the parent module for the string form collections use.

use std::cmp::Ordering;

/// The digits of `key`, right-padded with the insignificant trailing zeros that
/// a shorter fraction implies, so two keys can be compared digit for digit.
fn digits(key: &[u8], width: usize) -> impl Iterator<Item = u8> + '_ {
    key.iter().copied().chain(std::iter::repeat(0)).take(width)
}

/// Compare two keys as base-256 fractions (missing/trailing digits are 0).
pub fn cmp(a: &[u8], b: &[u8]) -> Ordering {
    let width = a.len().max(b.len());
    digits(a, width).cmp(digits(b, width))
}

/// `(lo + hi) / 2` as a base-256 fraction (requires `lo < hi`), so the result is
/// the exact average and therefore strictly between `lo` and `hi`.
///
/// Two passes, because a single most-significant-first pass cannot know whether
/// the digit it is about to emit will be pushed over 255 by a carry arriving
/// from the digit below it. So: add the two fractions least-significant-first,
/// where carries travel the way they can be absorbed, then halve the sum
/// most-significant-first, where the dropped low bit travels down as the 128
/// that the next digit owes.
fn midpoint(lo: &[u8], hi: &[u8]) -> Vec<u8> {
    let width = lo.len().max(hi.len()).max(1);

    // `lo + hi`, least-significant digit first. Both inputs are fractions below
    // 1, so the carry out of the top digit is the whole integer part: 0 or 1.
    let mut sum = Vec::with_capacity(width);
    let mut carry = 0u8;
    for i in (0..width).rev() {
        let l = u16::from(lo.get(i).copied().unwrap_or(0));
        let h = u16::from(hi.get(i).copied().unwrap_or(0));
        // Two digits plus a carry bit is at most 511, so the widening to u16
        // makes this addition exact and the saturation unreachable.
        let total = l.saturating_add(h).saturating_add(u16::from(carry));
        let [digit, next_carry] = total.to_le_bytes();
        sum.push(digit);
        carry = next_carry;
    }

    // Halve, most-significant digit first: each digit's dropped low bit is worth
    // half of 256 in the digit below, i.e. 128.
    let mut out = Vec::with_capacity(sum.len().saturating_add(1));
    let mut half_owed = carry == 1;
    for digit in sum.into_iter().rev() {
        out.push(if half_owed { 128 } else { 0 } | (digit / 2));
        half_owed = digit % 2 == 1;
    }
    if half_owed {
        out.push(128);
    }

    while out.len() > 1 && out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// Produce a key ordered strictly between `lo` and `hi` (either may be `None`
/// for "before everything" / "after everything").
pub fn between(lo: Option<&[u8]>, hi: Option<&[u8]>) -> Vec<u8> {
    match (lo, hi) {
        (None, None) => vec![128],
        (Some(l), None) => after(l),
        (None, Some(h)) => midpoint(&[], h),
        (Some(l), Some(h)) => midpoint(l, h),
    }
}

/// Digits an appended key counts in: appending after a key shorter than this counts in its
/// last digit, so that many bytes hold about `256^APPEND_DIGITS` appends before one more
/// group of digits is needed.
const APPEND_DIGITS: usize = 3;

/// A key strictly greater than `l`, for "append after `l`" (no upper bound).
///
/// Counts: `l`, padded to at least [`APPEND_DIGITS`] digits, plus one in its last digit, with
/// the carry running up through digits already at 255 (dropped, as trailing zeros are
/// insignificant). Only when every digit is 255 does the key grow, by another
/// [`APPEND_DIGITS`] digits. A run of `n` appends therefore keeps keys O(log n) bytes long —
/// incrementing only the existing digits would exhaust a one-byte key after 127 appends and
/// grow a byte every ~128 more, making keys, every [`cmp`] and the order index O(n).
/// Concurrent appends after the same `l` still converge (ties broken by block id downstream).
fn after(l: &[u8]) -> Vec<u8> {
    let mut v = l.to_vec();
    while v.len() > 1 && v.last() == Some(&0) {
        v.pop();
    }
    if v.len() < APPEND_DIGITS {
        v.resize(APPEND_DIGITS, 0);
    }
    if let Some(i) = v.iter().rposition(|&b| b < 255) {
        v.truncate(i.saturating_add(1));
        if let Some(last) = v.last_mut() {
            // `rposition` chose this digit because it is below 255, so the
            // increment is exact and the saturation is unreachable.
            *last = last.saturating_add(1);
        }
        return v;
    }
    // Every digit is 255 (≈ 1.0): nothing this long exceeds it, so count in fresh digits.
    v.resize(v.len().saturating_add(APPEND_DIGITS), 0);
    if let Some(last) = v.last_mut() {
        *last = 1;
    }
    v
}

/// A key strictly between `lo` and `hi` that differs with `salt`, so two
/// replicas inserting into the same gap at once, each with its own salt,
/// almost always get distinct keys, and a later insert fits between them.
///
/// Between two bounds it lies a salted fraction of the way across, within
/// the middle half of the gap; after the last key it counts one up in the
/// digit before two salt digits, so a run of appends keeps its length.
pub fn between_salted(lo: Option<&[u8]>, hi: Option<&[u8]>, salt: u16) -> Vec<u8> {
    match hi {
        None => after_salted(lo.unwrap_or(&[]), salt),
        Some(h) => across(lo.unwrap_or(&[]), h, salt),
    }
}

/// `lo + (hi - lo) * f` for `f` in `[1/4, 3/4)` chosen by `salt` (requires
/// `lo < hi`).
fn across(lo: &[u8], hi: &[u8], salt: u16) -> Vec<u8> {
    let width = lo.len().max(hi.len()).max(1);
    // `hi - lo`, least-significant digit first; `hi > lo`, so nothing is
    // borrowed out of the top digit.
    let mut gap = vec![0u8; width];
    let mut borrow = 0u16;
    for i in (0..width).rev() {
        let h = u16::from(hi.get(i).copied().unwrap_or(0));
        let taken = u16::from(lo.get(i).copied().unwrap_or(0)).saturating_add(borrow);
        // A digit plus a borrow is at most 256, so the widened sums are exact.
        let (digit, next) = if h >= taken {
            (h.saturating_sub(taken), 0)
        } else {
            (h.saturating_add(256).saturating_sub(taken), 1)
        };
        borrow = next;
        if let Some(slot) = gap.get_mut(i) {
            *slot = u8::try_from(digit).unwrap_or(0);
        }
    }
    // `gap * n / 65536`, with `n` in `[16384, 49152)`: two digits longer.
    let n = 16_384u32.saturating_add(u32::from(salt % 32_768));
    let mut part = vec![0u8; width.saturating_add(2)];
    let mut carry = 0u32;
    for i in (0..width).rev() {
        // A digit times `n` plus a carry stays far below `u32::MAX`.
        let value = u32::from(gap.get(i).copied().unwrap_or(0))
            .saturating_mul(n)
            .saturating_add(carry);
        if let Some(slot) = part.get_mut(i.saturating_add(2)) {
            *slot = u8::try_from(value & 0xff).unwrap_or(0);
        }
        carry = value >> 8;
    }
    // The product is below 1, so what is carried out is the two top digits.
    if let Some(slot) = part.get_mut(1) {
        *slot = u8::try_from(carry & 0xff).unwrap_or(0);
    }
    if let Some(slot) = part.first_mut() {
        *slot = u8::try_from((carry >> 8) & 0xff).unwrap_or(0);
    }
    // `lo + part`, below `hi` and so below 1: nothing carries out.
    let mut out = vec![0u8; part.len()];
    let mut carry = 0u16;
    for i in (0..part.len()).rev() {
        let total = u16::from(lo.get(i).copied().unwrap_or(0))
            .saturating_add(u16::from(part.get(i).copied().unwrap_or(0)))
            .saturating_add(carry);
        let [digit, next] = total.to_le_bytes();
        if let Some(slot) = out.get_mut(i) {
            *slot = digit;
        }
        carry = u16::from(next);
    }
    while out.len() > 1 && out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// A key after `l` ending in two salt digits.
fn after_salted(l: &[u8], salt: u16) -> Vec<u8> {
    let [high, low] = salt.to_be_bytes();
    let salted = |mut head: Vec<u8>| {
        // The last digit is never zero, so the key keeps its length.
        head.extend_from_slice(&[high, low | 1]);
        head
    };
    let mut v = l.to_vec();
    while v.len() > 1 && v.last() == Some(&0) {
        v.pop();
    }
    if v.len() < APPEND_DIGITS.saturating_add(2) {
        v.resize(APPEND_DIGITS.saturating_add(2), 0);
    }
    let head = v.len().saturating_sub(2);
    let mut counted = v.get(..head).unwrap_or(&[]).to_vec();
    if let Some(i) = counted.iter().rposition(|&b| b < 255) {
        counted.truncate(i.saturating_add(1));
        if let Some(last) = counted.last_mut() {
            *last = last.saturating_add(1);
        }
        counted.resize(head, 0);
        return salted(counted);
    }
    // Every counted digit is 255: count on past all of `l`.
    let mut longer = v;
    longer.extend(std::iter::repeat_n(0, APPEND_DIGITS.saturating_sub(1)));
    longer.push(1);
    salted(longer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salted_keys_stay_within_their_bounds_and_differ_by_salt() {
        use Ordering::Less;
        type Bound = Option<Vec<u8>>;
        let bounds: [(Bound, Bound); 6] = [
            (None, None),
            (Some(vec![10]), Some(vec![11])),
            (Some(vec![10, 255, 255]), Some(vec![11])),
            (None, Some(vec![0, 0, 1])),
            (Some(vec![255, 255, 255, 255, 255]), None),
            (
                Some(vec![1, 2, 3, 4, 5, 6, 7]),
                Some(vec![1, 2, 3, 4, 5, 6, 8]),
            ),
        ];
        for (lo, hi) in bounds {
            let mut keys = Vec::new();
            for salt in [0, 2, 7_000, 20_001, 32_767] {
                let key = between_salted(lo.as_deref(), hi.as_deref(), salt);
                if let Some(lo) = &lo {
                    assert_eq!(cmp(lo, &key), Less, "{lo:?} < {key:?}");
                }
                if let Some(hi) = &hi {
                    assert_eq!(cmp(&key, hi), Less, "{key:?} < {hi:?}");
                }
                keys.push(key);
            }
            let mut distinct = keys.clone();
            distinct.sort();
            distinct.dedup();
            assert_eq!(distinct.len(), keys.len(), "{keys:?}");
        }
    }

    #[test]
    fn a_later_insert_fits_between_two_made_at_once() {
        let (lo, hi) = (vec![64u8], vec![192u8]);
        let one = between_salted(Some(&lo), Some(&hi), 11);
        let other = between_salted(Some(&lo), Some(&hi), 12_345);
        let (first, second) = if cmp(&one, &other).is_lt() {
            (one, other)
        } else {
            (other, one)
        };
        let middle = between_salted(Some(&first), Some(&second), 99);
        assert!(cmp(&first, &middle).is_lt() && cmp(&middle, &second).is_lt());
    }

    #[test]
    fn a_run_of_salted_appends_keeps_its_length() {
        let mut key = between_salted(None, None, 5);
        let length = key.len();
        for salt in 0..2_000u16 {
            let next = between_salted(Some(&key), None, salt.wrapping_mul(7_919));
            assert!(cmp(&key, &next).is_lt());
            key = next;
        }
        assert_eq!(key.len(), length);
    }

    #[test]
    fn between_orders_strictly_within_its_bounds() {
        use Ordering::{Equal, Less};
        let mid = between(None, None);
        let after = between(Some(&mid), None);
        let before = between(None, Some(&mid));
        let (ten, twenty) = (vec![10u8], vec![20u8]);
        let midpoint = between(Some(&ten), Some(&twenty));
        for (name, a, b, want) in [
            (
                "between empty bounds is past the empty key",
                &[][..],
                &mid[..],
                Less,
            ),
            ("append after is greater", &mid[..], &after[..], Less),
            ("prepend before is smaller", &before[..], &mid[..], Less),
            (
                "midpoint is above its lower bound",
                &ten[..],
                &midpoint[..],
                Less,
            ),
            (
                "midpoint is below its upper bound",
                &midpoint[..],
                &twenty[..],
                Less,
            ),
            (
                "trailing zeros compare equal",
                &[5][..],
                &[5, 0, 0][..],
                Equal,
            ),
        ] {
            assert_eq!(cmp(a, b), want, "case {name}");
        }

        // Insert 200 keys always between the first two; each must stay ordered.
        let mut lo = between(None, None);
        let hi = between(Some(&lo), None);
        let mut prev = lo.clone();
        for _ in 0..200 {
            let m = between(Some(&lo), Some(&hi));
            assert_eq!(cmp(&lo, &m), Less, "m must exceed lo");
            assert_eq!(cmp(&m, &hi), Less, "m must precede hi");
            assert_eq!(cmp(&prev, &m), Less, "strictly increasing");
            prev = m.clone();
            lo = m;
        }
    }

    #[test]
    fn sequential_appends_stay_short_and_ordered() {
        // A long run of appends (the book-length-document case) must NOT grow keys
        // one byte per insert — that is the O(n²) trap (every cmp is O(len)).
        let mut key = between(None, None);
        let mut max_len = key.len();
        for _ in 0..1_000_000 {
            let next = between(Some(&key), None);
            assert_eq!(
                cmp(&key, &next),
                Ordering::Less,
                "each append is strictly greater"
            );
            key = next;
            max_len = max_len.max(key.len());
        }
        // Logarithmic: a million appends fit in three digits.
        assert!(
            max_len <= APPEND_DIGITS,
            "append keys must stay short (got {max_len} bytes) or insertion is O(n²)"
        );
    }

    #[test]
    fn random_inserts_stay_strictly_ordered() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..64 {
            let mut keys: Vec<Vec<u8>> = vec![between(None, None)];
            for _ in 0..120 {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let slot = usize::try_from(seed >> 40).unwrap_or(0);
                let idx = slot.checked_rem(keys.len().saturating_add(1)).unwrap_or(0);
                let lo = idx
                    .checked_sub(1)
                    .and_then(|i| keys.get(i))
                    .map(Vec::as_slice);
                let hi = keys.get(idx).map(Vec::as_slice);
                let k = between(lo, hi);
                if let Some(l) = lo {
                    assert_eq!(cmp(l, &k), Ordering::Less);
                }
                if let Some(h) = hi {
                    assert_eq!(cmp(&k, h), Ordering::Less);
                }
                keys.insert(idx, k);
            }
            for w in keys.windows(2) {
                assert_eq!(cmp(&w[0], &w[1]), Ordering::Less);
            }
        }
    }
}
