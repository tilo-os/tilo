//! qOS load-path hardening: framing checks for uncompressed bundle columns.
//!
//! The bundle iterators read columns with hexane's streaming decoders, which
//! assume validated input: `RleValue::unpack` unwraps `try_unpack` (panic on
//! a truncated varint or an out-of-range `Action`), `String::unpack` builds a
//! `&str` with `from_utf8_unchecked`, a null run in a non-nullable column
//! panics in `get_null`, the delta decoders add with `+=` (overflow panic in
//! debug builds), and a truncated boolean run count makes `BoolDecoder::next`
//! spin forever without advancing. Documents do not have this problem
//! because `Column::load` validates every value; bundles skip that step.
//!
//! These functions walk a column once, with exactly the run grammar the
//! streaming decoders accept (non-canonical runs included, so every bundle
//! that loaded before still loads), and refuse anything the decoders would
//! choke on. They allocate nothing.
use hexane::{Codec, Leb128, PackError, RleValue};

/// RLE column read by `hexane::decoder::<T>`.
pub(super) fn rle<T: RleValue>(data: &[u8]) -> Result<(), PackError> {
    walk_rle::<T>(data, |_| Ok(()))
}

/// Delta column read by `hexane::DeltaDecoder::<Option<i64>>`: an RLE
/// column of `Option<i64>` deltas whose running sum must stay in `i64`.
pub(super) fn delta(data: &[u8]) -> Result<(), PackError> {
    let mut running: i64 = 0;
    walk_rle::<Option<i64>>(data, |step| {
        if let (Some(delta), count) = step {
            let total = i64::try_from(count)
                .ok()
                .and_then(|count| delta.checked_mul(count))
                .ok_or(PackError::BadFormat)?;
            running = running.checked_add(total).ok_or(PackError::BadFormat)?;
        }
        Ok(())
    })
}

/// Boolean column read by `hexane::decoder::<bool>`: alternating run counts.
pub(super) fn boolean(mut data: &[u8]) -> Result<(), PackError> {
    while !data.is_empty() {
        let (n, _) = Leb128::try_read_unsigned(data)?;
        data = &data[n..];
    }
    Ok(())
}

/// Walks the RLE grammar of `hexane::rle::RleDecoder`: a signed run header,
/// then one value (repeat, `n > 0`), `-n` values (literal, `n < 0`), or an
/// unsigned null count (`n == 0`). `each` sees `(value, times)` for every
/// repeat or literal value, with `times` its multiplicity.
fn walk_rle<'a, T: RleValue>(
    mut data: &'a [u8],
    mut each: impl FnMut((T::Get<'a>, u64)) -> Result<(), PackError>,
) -> Result<(), PackError> {
    while !data.is_empty() {
        let (n, run) = Leb128::try_read_signed(data)?;
        data = &data[n..];
        if run > 0 {
            let (n, value) = T::try_unpack::<Leb128>(data)?;
            data = &data[n..];
            each((value, run.unsigned_abs()))?;
        } else if run < 0 {
            // Every value takes at least one byte, so this loop is bounded
            // by the column length even for a huge declared literal count.
            for _ in 0..run.unsigned_abs() {
                if data.is_empty() {
                    return Err(PackError::BadFormat);
                }
                let (n, value) = T::try_unpack::<Leb128>(data)?;
                data = &data[n..];
                each((value, 1))?;
            }
        } else {
            let (n, _) = Leb128::try_read_unsigned(data)?;
            data = &data[n..];
            if !T::NULLABLE {
                return Err(PackError::BadFormat);
            }
        }
    }
    Ok(())
}

