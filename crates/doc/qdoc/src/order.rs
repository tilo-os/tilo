//! Fractional order keys: base-256 fractions where a midpoint always exists
//! between two distinct keys. Compared as fractions (trailing zeros are
//! insignificant), so block order survives concurrent structural edits.

use std::cmp::Ordering;

/// The digits of `key`, right-padded with the insignificant trailing zeros that
/// a shorter fraction implies, so two keys can be compared digit for digit.
fn digits(key: &[u8], width: usize) -> impl Iterator<Item = u8> + '_ {
    key.iter().copied().chain(std::iter::repeat(0)).take(width)
}

/// Compare two keys as base-256 fractions (missing/trailing digits are 0).
pub fn cmp_keys(a: &[u8], b: &[u8]) -> Ordering {
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
pub fn key_between(lo: Option<&[u8]>, hi: Option<&[u8]>) -> Vec<u8> {
    match (lo, hi) {
        (None, None) => vec![128],
        (Some(l), None) => key_after(l),
        (None, Some(h)) => midpoint(&[], h),
        (Some(l), Some(h)) => midpoint(l, h),
    }
}

/// A key strictly greater than `l`, for "append after `l`" (no upper bound). A naive `l + [128]`
/// grows the key by a byte on every append, making a run of `n` appends O(n) bytes long and every
/// [`cmp_keys`] O(key length) — O(n²) to build/sort `n` blocks. This instead increments `l` as a
/// base-256 value and truncates, so keys grow ~1 byte per 128 appends (128× shorter), turning the
/// quadratic back into near-linear; the result is still strictly greater than `l`, and concurrent appends after the same `l` still converge (ties broken by block id downstream).
fn key_after(l: &[u8]) -> Vec<u8> {
    let mut v = l.to_vec();
    // Trailing zeros are insignificant as a fraction; drop them so the increment
    // lands on a significant digit (keeps the result minimal).
    while v.len() > 1 && v.last() == Some(&0) {
        v.pop();
    }
    // Increment the least-significant byte that isn't already 0xFF, dropping the
    // bytes after it — the shortest fraction greater than `l` at that position.
    if let Some(i) = v.iter().rposition(|&b| b < 255) {
        v.truncate(i.saturating_add(1));
        if let Some(last) = v.last_mut() {
            // `rposition` chose this byte because it is below 255, so the
            // increment is exact and the saturation is unreachable.
            *last = last.saturating_add(1);
        }
        return v;
    }
    // `l` is all 0xFF (≈ 1.0): no shorter key exceeds it, so extend by one mid byte.
    let mut out = l.to_vec();
    out.push(128);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn between_empty_bounds_is_mid() {
        let k = key_between(None, None);
        assert_eq!(cmp_keys(&[], &k), Ordering::Less);
    }

    #[test]
    fn append_after_is_greater() {
        let a = key_between(None, None);
        let b = key_between(Some(&a), None);
        assert_eq!(cmp_keys(&a, &b), Ordering::Less);
    }

    #[test]
    fn prepend_before_is_smaller() {
        let a = key_between(None, None);
        let b = key_between(None, Some(&a));
        assert_eq!(cmp_keys(&b, &a), Ordering::Less);
    }

    #[test]
    fn midpoint_is_strictly_between() {
        let a = vec![10u8];
        let b = vec![20u8];
        let m = key_between(Some(&a), Some(&b));
        assert_eq!(cmp_keys(&a, &m), Ordering::Less);
        assert_eq!(cmp_keys(&m, &b), Ordering::Less);
    }

    #[test]
    fn repeated_subdivision_stays_ordered() {
        // Insert 200 keys always between the first two; each must stay ordered.
        let mut lo = key_between(None, None);
        let hi = key_between(Some(&lo), None);
        let mut prev = lo.clone();
        for _ in 0..200 {
            let m = key_between(Some(&lo), Some(&hi));
            assert_eq!(cmp_keys(&lo, &m), Ordering::Less, "m must exceed lo");
            assert_eq!(cmp_keys(&m, &hi), Ordering::Less, "m must precede hi");
            assert_eq!(cmp_keys(&prev, &m), Ordering::Less, "strictly increasing");
            prev = m.clone();
            lo = m;
        }
    }

    #[test]
    fn trailing_zeros_compare_equal() {
        assert_eq!(cmp_keys(&[5], &[5, 0, 0]), Ordering::Equal);
    }

    #[test]
    fn sequential_appends_stay_short_and_ordered() {
        // A long run of appends (the book-length-document case) must NOT grow keys
        // one byte per insert — that is the O(n²) trap (every cmp_keys is O(len)).
        let mut key = key_between(None, None);
        let mut max_len = key.len();
        for _ in 0..4_000 {
            let next = key_between(Some(&key), None);
            assert_eq!(
                cmp_keys(&key, &next),
                Ordering::Less,
                "each append is strictly greater"
            );
            key = next;
            max_len = max_len.max(key.len());
        }
        // ~128 appends per byte of width → ~32 bytes for 4000, versus a naive ~4000.
        assert!(
            max_len <= 40,
            "append keys must stay short (got {max_len} bytes) or insertion is O(n²)"
        );
    }

    use proptest::prelude::*;

    proptest! {
        // Insert at random gaps among a growing sorted key list; it must stay strictly ordered and every gap must remain subdividable.
        #[test]
        fn random_inserts_stay_strictly_ordered(ops in proptest::collection::vec(any::<u8>(), 0..120)) {
            let mut keys: Vec<Vec<u8>> = vec![key_between(None, None)];
            for byte in ops {
                // One slot per gap plus one past the end; never zero, so the
                // fallback is unreachable (and 0 would be a valid slot anyway).
                let idx = usize::from(byte)
                    .checked_rem(keys.len().saturating_add(1))
                    .unwrap_or(0);
                let lo = idx.checked_sub(1).and_then(|i| keys.get(i)).map(Vec::as_slice);
                let hi = keys.get(idx).map(Vec::as_slice);
                let k = key_between(lo, hi);
                if let Some(l) = lo { prop_assert_eq!(cmp_keys(l, &k), Ordering::Less); }
                if let Some(h) = hi { prop_assert_eq!(cmp_keys(&k, h), Ordering::Less); }
                keys.insert(idx, k);
            }
            for w in keys.windows(2) {
                prop_assert_eq!(cmp_keys(&w[0], &w[1]), Ordering::Less);
            }
        }
    }
}
