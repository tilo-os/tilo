//! Fractional order keys: base-256 fractions where a midpoint always exists
//! between two distinct keys. Compared as fractions (trailing zeros are
//! insignificant), so block order survives concurrent structural edits.

use std::cmp::Ordering;

/// Compare two keys as base-256 fractions (missing/trailing digits are 0).
pub fn cmp_keys(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let da = a.get(i).copied().unwrap_or(0);
        let db = b.get(i).copied().unwrap_or(0);
        match da.cmp(&db) {
            Ordering::Equal => continue,
            ord => return ord,
        }
    }
    Ordering::Equal
}

/// `(lo + hi) / 2` as a base-256 fraction. Requires `lo < hi`.
/// Processes most-significant-first, carrying the division remainder down,
/// plus one extra digit to absorb the final remainder — so the result is the
/// exact average and therefore strictly between `lo` and `hi`.
fn midpoint(lo: &[u8], hi: &[u8]) -> Vec<u8> {
    let n = lo.len().max(hi.len()) + 1;
    let mut out = Vec::with_capacity(n);
    let mut rem: u16 = 0;
    for i in 0..n {
        let l = lo.get(i).copied().unwrap_or(0) as u16;
        let h = hi.get(i).copied().unwrap_or(0) as u16;
        let total = rem * 256 + l + h;
        out.push((total / 2) as u8);
        rem = total % 2;
    }
    while out.len() > 1 && *out.last().unwrap() == 0 {
        out.pop();
    }
    out
}

/// Produce a key ordered strictly between `lo` and `hi` (either may be `None`
/// for "before everything" / "after everything").
pub fn key_between(lo: Option<&[u8]>, hi: Option<&[u8]>) -> Vec<u8> {
    match (lo, hi) {
        (None, None) => vec![128],
        (Some(l), None) => {
            let mut v = l.to_vec();
            v.push(128);
            v
        }
        (None, Some(h)) => midpoint(&[], h),
        (Some(l), Some(h)) => midpoint(l, h),
    }
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

    use proptest::prelude::*;

    proptest! {
        // Insert at random gaps among a growing sorted key list; the list must
        // stay strictly ordered and every gap must remain subdividable.
        #[test]
        fn random_inserts_stay_strictly_ordered(ops in proptest::collection::vec(any::<u8>(), 0..120)) {
            let mut keys: Vec<Vec<u8>> = vec![key_between(None, None)];
            for byte in ops {
                let idx = (byte as usize) % (keys.len() + 1);
                let lo = if idx == 0 { None } else { Some(keys[idx - 1].as_slice()) };
                let hi = if idx == keys.len() { None } else { Some(keys[idx].as_slice()) };
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
