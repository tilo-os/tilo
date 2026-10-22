//! Smart fill — the fill-handle / autofill engine.
//!
//! Given the run of cells the user selected as a *pattern*, extend it the way
//! Excel and Google Sheets do when the fill handle is dragged. The pattern's
//! shape picks the heuristic:
//!
//!   * **formulas** copy forward with their relative references shifted
//!     (`=A1` → `=A2` → `=A3`), preserving `$`-anchors — see
//!     [`crate::formula::offset_refs`];
//!   * **a single number** fills as a constant (`5` → `5`, `5`, …);
//!   * **two or more numbers** extrapolate a linear series (`1, 2` → `3, 4,
//!     5`; a straight line fit for longer runs);
//!   * **dates** continue by the detected step — a whole-month cadence stays
//!     monthly (`31 Jan, 29 Feb` style day-clamping included), otherwise a
//!     constant day delta (a lone date advances one day at a time);
//!   * **month or weekday names** cycle onward (`Jan` → `Feb`; `Mon, Wed` →
//!     `Fri, Sun`);
//!   * **text with a trailing number** increments it (`Item 1` → `Item 2`),
//!     keeping any zero-padding (`Q01` → `Q02`);
//!   * **anything else** (pure text, blanks, mixed blocks) repeats the block
//!     verbatim.
//!
//! The engine is pure: it takes the pattern cells and a target count and
//! returns the cells to write, so it is exercised in isolation by the unit
//! tests here and through the real fill-handle event path in `app`.

use crate::model::{CellFormat, CellValue, date_to_serial, serial_to_ymd};
use ui::num::f64_of_usize;

/// One source cell handed to the fill engine: the stored raw input, the
/// computed value (used to recognise numeric / date series), and the display
/// format (carried onto the generated cells).
#[derive(Clone, Debug, PartialEq)]
pub struct FillCell {
    pub raw: String,
    pub value: CellValue,
    pub fmt: CellFormat,
}

impl FillCell {
    /// Convenience constructor for tests and callers.
    pub fn new(raw: impl Into<String>, value: CellValue, fmt: CellFormat) -> Self {
        Self {
            raw: raw.into(),
            value,
            fmt,
        }
    }

    fn is_formula(&self) -> bool {
        self.raw.trim_start().starts_with('=')
    }

    /// The plain display string used for text-shaped detection (names,
    /// text+number). Prefers the computed value, falling back to the raw.
    fn display(&self) -> String {
        match &self.value {
            CellValue::Empty => self.raw.trim().to_string(),
            v => v.to_plain_string(),
        }
    }
}

/// One generated cell: the raw to write and the format to set.
#[derive(Clone, Debug, PartialEq)]
pub struct FillOut {
    pub raw: String,
    pub fmt: CellFormat,
}

/// Extend `pattern` by `count` new cells continuing forward in the fill
/// direction `(dcol, drow)` — `(0, 1)` fills down, `(1, 0)` fills right.
/// Returns exactly `count` cells, in forward order.
pub fn extend_series(pattern: &[FillCell], count: usize, dcol: i64, drow: i64) -> Vec<FillOut> {
    if pattern.is_empty() || count == 0 {
        return Vec::new();
    }
    // Any formula in the run → copy the block forward, shifting refs.
    if pattern.iter().any(FillCell::is_formula) {
        return block_copy(pattern, count, dcol, drow);
    }
    if let Some(out) = numeric_series(pattern, count) {
        return out;
    }
    if let Some(out) = date_series(pattern, count) {
        return out;
    }
    if let Some(out) = name_series(pattern, count) {
        return out;
    }
    if let Some(out) = text_number_series(pattern, count) {
        return out;
    }
    // Pure text / blanks / mixed → repeat the block verbatim.
    block_copy(pattern, count, dcol, drow)
}

/// Copy the pattern block forward, repeating it, shifting each formula's
/// relative references by how far the generated cell sits from the source it
/// copies. Plain values copy verbatim.
fn block_copy(pattern: &[FillCell], count: usize, dcol: i64, drow: i64) -> Vec<FillOut> {
    let block = i64::try_from(pattern.len()).unwrap_or(i64::MAX);
    // Distance from a generated cell back to the source it copies: one whole
    // block for the first repetition, one more for each further one.
    let mut steps = block;
    let mut sources = pattern.iter();
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if sources.as_slice().is_empty() {
            // Wrapped past the end of the block: this repetition sits one
            // whole block further from the source it copies.
            steps = steps.saturating_add(block);
            sources = pattern.iter();
        }
        let Some(src) = sources.next() else {
            break; // empty pattern: nothing to copy forward
        };
        let raw = if src.is_formula() {
            crate::formula::offset_refs(
                &src.raw,
                steps.saturating_mul(dcol),
                steps.saturating_mul(drow),
            )
        } else {
            src.raw.clone()
        };
        out.push(FillOut { raw, fmt: src.fmt });
    }
    out
}

/// Plain-number series: every cell is a number that is not date/time
/// formatted. One number fills constant; two or more extrapolate a line.
fn numeric_series(pattern: &[FillCell], count: usize) -> Option<Vec<FillOut>> {
    let mut nums = Vec::with_capacity(pattern.len());
    for c in pattern {
        if matches!(c.fmt, CellFormat::Date(_) | CellFormat::Time(_)) {
            return None;
        }
        match &c.value {
            CellValue::Number(n) => nums.push(*n),
            _ => return None,
        }
    }
    let len = nums.len();
    // Formats cycle through the pattern as the series runs past its end.
    Some(
        pattern
            .iter()
            .cycle()
            .zip(0..count)
            .map(|(src, j)| FillOut {
                raw: num_to_raw(linear_at(&nums, len.saturating_add(j))),
                fmt: src.fmt,
            })
            .collect(),
    )
}

/// Date series: every cell is a date-formatted number. A consistent
/// whole-month cadence stays monthly; otherwise a constant day delta (a lone
/// date advances one day per step).
fn date_series(pattern: &[FillCell], count: usize) -> Option<Vec<FillOut>> {
    let mut serials = Vec::with_capacity(pattern.len());
    let mut style = None;
    for c in pattern {
        let CellFormat::Date(s) = c.fmt else {
            return None;
        };
        let CellValue::Number(n) = c.value else {
            return None;
        };
        serials.push(n.floor());
        style = Some(s);
    }
    let style = style?;
    let len = serials.len();

    if let Some(months) = monthly_step(&serials) {
        let (y, m, d) = serial_to_ymd(*serials.last()?);
        return Some(
            (0..count)
                .map(|j| {
                    // `j` counts generated cells, so the month offset stays
                    // small; saturating keeps an absurd drag from wrapping.
                    let ahead = i64::try_from(j).unwrap_or(i64::MAX).saturating_add(1);
                    let serial = add_months(y, m, d, ahead.saturating_mul(months));
                    FillOut {
                        raw: num_to_raw(serial),
                        fmt: CellFormat::Date(style),
                    }
                })
                .collect(),
        );
    }

    Some(
        (0..count)
            .map(|j| {
                let serial = series_at(&serials, len.saturating_add(j)).round();
                FillOut {
                    raw: num_to_raw(serial),
                    fmt: CellFormat::Date(style),
                }
            })
            .collect(),
    )
}

/// Month-name or weekday-name series: every cell is a recognised name from a
/// single list; the sequence continues cycling with the detected step
/// (default +1).
fn name_series(pattern: &[FillCell], count: usize) -> Option<Vec<FillOut>> {
    // format: machine (the names a cell holds and the series continues; never
    // the viewer's language, so every collaborator fills the same cells)
    const MONTHS_SHORT: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // format: machine (as above)
    const MONTHS_LONG: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // format: machine (as above)
    const DAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    // format: machine (as above)
    const DAYS_LONG: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];

    for list in [
        &MONTHS_SHORT[..],
        &MONTHS_LONG[..],
        &DAYS_SHORT[..],
        &DAYS_LONG[..],
    ] {
        if let Some(idxs) = match_names(pattern, list) {
            let Ok(n) = i64::try_from(list.len()) else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let step = mod_series_step(&idxs, n);
            let last = *idxs.last()?;
            return Some(
                pattern
                    .iter()
                    .cycle()
                    .zip(0..count)
                    .map(|(src, j)| {
                        // `rem_euclid(n)` lands inside the list, so the lookup
                        // below always finds a name.
                        let ahead = i64::try_from(j).unwrap_or(i64::MAX).saturating_add(1);
                        let idx = last
                            .saturating_add(step.saturating_mul(ahead))
                            .rem_euclid(n);
                        let name = usize::try_from(idx)
                            .ok()
                            .and_then(|i| list.get(i))
                            .copied()
                            .unwrap_or_default();
                        FillOut {
                            raw: name.to_string(),
                            fmt: src.fmt,
                        }
                    })
                    .collect(),
            );
        }
    }
    None
}

/// Text-with-trailing-number series: every cell shares a common leading
/// prefix and ends in an integer; the numbers extrapolate (a single cell
/// increments by one), keeping any zero-padding.
fn text_number_series(pattern: &[FillCell], count: usize) -> Option<Vec<FillOut>> {
    let mut prefix: Option<String> = None;
    let mut width = usize::MAX;
    let mut nums = Vec::with_capacity(pattern.len());
    for c in pattern {
        let (pre, val, w) = split_trailing_number(&c.display())?;
        match &prefix {
            None => prefix = Some(pre),
            Some(p) if *p == pre => {}
            _ => return None,
        }
        width = width.min(w);
        nums.push(val);
    }
    let prefix = prefix?;
    let len = nums.len();
    Some(
        pattern
            .iter()
            .cycle()
            .zip(0..count)
            .map(|(src, j)| {
                let value = series_at(&nums, len.saturating_add(j)).round();
                // A small negative fit rounds to `-0.0`; fold it back so a
                // zero never renders with a sign.
                let value = if value == 0.0 { 0.0 } else { value };
                let num_str = if width > 1 && value >= 0.0 {
                    format!("{value:0width$.0}")
                } else {
                    format!("{value:.0}")
                };
                FillOut {
                    raw: format!("{prefix}{num_str}"),
                    fmt: src.fmt,
                }
            })
            .collect(),
    )
}

/// Extrapolated value at position `p` for a series where a single sample fills
/// constant. Two or more samples: an exact arithmetic progression continues by
/// its step; otherwise a least-squares straight-line fit.
fn linear_at(vals: &[f64], p: usize) -> f64 {
    match vals {
        [only] => *only,
        _ => line_value(vals, p),
    }
}

/// Like [`linear_at`] but a single sample *increments by one* per step — the
/// behaviour dates and text+number use (a lone `1/1` advances a day; a lone
/// `Item 1` becomes `Item 2`).
fn series_at(vals: &[f64], p: usize) -> f64 {
    match vals {
        [only] => only + f64_of_usize(p),
        _ => line_value(vals, p),
    }
}

/// The line through `vals` (positions `0..len`) evaluated at `p`. Uses the
/// exact step for an arithmetic progression, else least squares.
fn line_value(vals: &[f64], p: usize) -> f64 {
    let [first, second, ..] = vals else {
        // Fewer than two samples: there is no line, only the sample itself.
        return vals.first().copied().unwrap_or(0.0);
    };
    let len = vals.len();
    let step = second - first;
    let is_ap = vals
        .iter()
        .zip(vals.iter().skip(1))
        .all(|(a, b)| ((b - a) - step).abs() <= 1e-9 * (1.0 + a.abs().max(b.abs())));
    if is_ap {
        return first + step * f64_of_usize(p);
    }
    // Least-squares linear fit over positions 0..len.
    let n = f64_of_usize(len);
    let sx: f64 = (0..len).map(f64_of_usize).sum();
    let sy: f64 = vals.iter().sum();
    let sxx: f64 = (0..len)
        .map(|i| {
            let x = f64_of_usize(i);
            x * x
        })
        .sum();
    let sxy: f64 = vals
        .iter()
        .enumerate()
        .map(|(i, v)| f64_of_usize(i) * v)
        .sum();
    let n_sxx = n * sxx;
    let sx_squared = sx * sx;
    let denom = n_sxx - sx_squared;
    if denom.abs() < 1e-12 {
        return sy / n;
    }
    let slope = (n * sxy - sx * sy) / denom;
    let intercept = (sy - slope * sx) / n;
    intercept + slope * f64_of_usize(p)
}

/// Render a number as the shortest exact-looking decimal (matching how the
/// smart-entry path persists numbers): integers print bare, fractions keep up
/// to ten significant digits with trailing zeros trimmed.
fn num_to_raw(n: f64) -> String {
    if n == 0.0 {
        return "0".to_string(); // also normalises `-0.0`
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        // Whole and inside the exactly-representable range: print the integer
        // straight, with no decimal point.
        return format!("{n:.0}");
    }
    let s = format!("{n:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A consistent whole-month step across the serials (same day-of-month, equal
/// non-zero month deltas), or `None`. Needs at least two dates — a lone date
/// is treated as a daily series, matching Excel.
fn monthly_step(serials: &[f64]) -> Option<i64> {
    if serials.len() < 2 {
        return None;
    }
    let ymd: Vec<(i64, u32, u32)> = serials.iter().map(|s| serial_to_ymd(*s)).collect();
    let (_, _, d0) = *ymd.first()?;
    if ymd.iter().any(|&(_, _, d)| d != d0) {
        return None;
    }
    // The years come from a serial the model already bounds, so the month
    // index stays tiny and the saturating ops never trigger.
    let months: Vec<i64> = ymd
        .iter()
        .map(|&(y, m, _)| y.saturating_mul(12).saturating_add(i64::from(m)))
        .collect();
    let mut deltas = months
        .iter()
        .zip(months.iter().skip(1))
        .map(|(a, b)| b.saturating_sub(*a));
    let step = deltas.next()?;
    if step == 0 {
        return None;
    }
    deltas.all(|d| d == step).then_some(step)
}

/// Add `add` months to `(y, m, d)` and return the resulting Excel serial,
/// clamping the day into the target month (`31 Jan` + 1 month → `28/29 Feb`).
fn add_months(y: i64, m: u32, d: u32, add: i64) -> f64 {
    // Months since year 0. `y` comes from a serial the model already bounds
    // and `add` from the fill count, so saturating keeps an extreme drag from
    // wrapping into a bogus date.
    let total = y
        .saturating_mul(12)
        .saturating_add(i64::from(m))
        .saturating_sub(1)
        .saturating_add(add);
    let ny = total.div_euclid(12);
    // `rem_euclid(12)` is in [0, 11], so the month lands in [1, 12].
    let nm = u32::try_from(total.rem_euclid(12))
        .unwrap_or(0)
        .saturating_add(1);
    let dd = d.min(ui::date::days_in_month(ny, nm));
    date_to_serial(ny, nm, dd)
}

/// Every cell's display maps to an index in `list` (case-insensitive), or
/// `None`.
fn match_names(pattern: &[FillCell], list: &[&str]) -> Option<Vec<i64>> {
    let mut out = Vec::with_capacity(pattern.len());
    for c in pattern {
        let s = c.display();
        let s = s.trim();
        let idx = list.iter().position(|name| name.eq_ignore_ascii_case(s))?;
        out.push(i64::try_from(idx).ok()?);
    }
    Some(out)
}

/// The step of an index series taken modulo `wrap`: `+1` for a single sample,
/// else the consistent forward delta (or `+1` when inconsistent).
fn mod_series_step(idxs: &[i64], wrap: i64) -> i64 {
    // `rem_euclid` needs a positive modulus, and a lone sample has no delta.
    if wrap <= 0 {
        return 1;
    }
    let [first, second, ..] = idxs else {
        return 1;
    };
    let step = second.saturating_sub(*first).rem_euclid(wrap);
    let consistent = idxs
        .iter()
        .zip(idxs.iter().skip(1))
        .all(|(a, b)| b.saturating_sub(*a).rem_euclid(wrap) == step);
    if consistent && step != 0 { step } else { 1 }
}

/// Split a string into `(prefix, trailing_number, digit_width)`, or `None`
/// when it has no trailing digits — or so many that the value cannot be
/// represented, in which case there is no series to continue.
fn split_trailing_number(s: &str) -> Option<(String, f64, usize)> {
    // Every trailing digit is one ASCII byte, so the count is also the byte
    // offset of the split.
    let digits_len = s.chars().rev().take_while(char::is_ascii_digit).count();
    let split = s.len().checked_sub(digits_len)?;
    let (prefix, digits) = s.split_at_checked(split)?;
    if digits.is_empty() {
        return None; // no trailing digits
    }
    let val: f64 = digits.parse().ok()?;
    if !val.is_finite() {
        return None;
    }
    Some((prefix.to_string(), val, digits.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellFormat, CellValue};
    use tilo_locale::DateStyle;

    fn num(n: f64) -> FillCell {
        FillCell::new(num_to_raw(n), CellValue::Number(n), CellFormat::General)
    }
    fn text(s: &str) -> FillCell {
        FillCell::new(s, CellValue::Text(s.into()), CellFormat::Text)
    }
    fn date(y: i64, m: u32, d: u32) -> FillCell {
        let serial = date_to_serial(y, m, d);
        FillCell::new(
            num_to_raw(serial),
            CellValue::Number(serial),
            CellFormat::Date(DateStyle::Iso),
        )
    }
    fn raws(out: &[FillOut]) -> Vec<String> {
        out.iter().map(|o| o.raw.clone()).collect()
    }

    /// Formula reference shifting and linear/constant numeric series.
    #[test]
    fn numeric_and_formula_fills_extrapolate() {
        {
            // A single `=A1` filled down three cells → =A2, =A3, =A4.
            let pat = [FillCell::new(
                "=A1",
                CellValue::Number(0.0),
                CellFormat::General,
            )];
            assert_eq!(
                raws(&extend_series(&pat, 3, 0, 1)),
                vec!["=A2", "=A3", "=A4"]
            );
            // Filled right instead → =B1, =C1, =D1.
            assert_eq!(
                raws(&extend_series(&pat, 3, 1, 0)),
                vec!["=B1", "=C1", "=D1"]
            );
            let anch = [FillCell::new(
                "=$A$1+B1",
                CellValue::Number(0.0),
                CellFormat::General,
            )];
            assert_eq!(
                raws(&extend_series(&anch, 2, 0, 1)),
                vec!["=$A$1+B2", "=$A$1+B3"]
            );
        }

        {
            // 1, 2 → 3, 4, 5.
            let pat = [num(1.0), num(2.0)];
            assert_eq!(raws(&extend_series(&pat, 3, 0, 1)), vec!["3", "4", "5"]);
            // 2, 4 → 6, 8 (step 2).
            let pat = [num(2.0), num(4.0)];
            assert_eq!(raws(&extend_series(&pat, 2, 0, 1)), vec!["6", "8"]);
            // 10, 7 → 4, 1, -2 (negative step).
            let pat = [num(10.0), num(7.0)];
            assert_eq!(raws(&extend_series(&pat, 3, 0, 1)), vec!["4", "1", "-2"]);
        }

        {
            // A perfect line 2, 4, 6 → 8, 10.
            let pat = [num(2.0), num(4.0), num(6.0)];
            assert_eq!(raws(&extend_series(&pat, 2, 0, 1)), vec!["8", "10"]);
        }

        {
            let pat = [num(7.0)];
            assert_eq!(raws(&extend_series(&pat, 3, 0, 1)), vec!["7", "7", "7"]);
        }
    }

    /// Daily/monthly dates, month/weekday names, numbered text, pure text and mixed blocks.
    #[test]
    fn date_name_and_text_fills_continue_their_pattern() {
        {
            let pat = [date(2024, 1, 1)];
            let out = extend_series(&pat, 2, 0, 1);
            assert_eq!(out.len(), 2);
            for o in &out {
                assert!(matches!(o.fmt, CellFormat::Date(_)));
            }
            let s0: f64 = out[0].raw.parse().unwrap();
            let s1: f64 = out[1].raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(s0, DateStyle::Iso),
                "2024-01-02"
            );
            assert_eq!(
                crate::model::serial_to_date(s1, DateStyle::Iso),
                "2024-01-03"
            );
            let pat = [date(2024, 1, 1), date(2024, 1, 8)];
            let out = extend_series(&pat, 1, 0, 1);
            let s: f64 = out[0].raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(s, DateStyle::Iso),
                "2024-01-15"
            );
        }

        {
            // 15 Jan, 15 Feb → 15 Mar, 15 Apr (monthly cadence).
            let pat = [date(2024, 1, 15), date(2024, 2, 15)];
            let out = extend_series(&pat, 2, 0, 1);
            let s0: f64 = out[0].raw.parse().unwrap();
            let s1: f64 = out[1].raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(s0, DateStyle::Iso),
                "2024-03-15"
            );
            assert_eq!(
                crate::model::serial_to_date(s1, DateStyle::Iso),
                "2024-04-15"
            );
            // Month-end clamps: a day-31 monthly cadence (Oct 31, Dec 31 — step of
            // two months) continues to Feb 29 (leap) and Apr 30, each clamped into
            // its shorter month.
            let pat = [date(2023, 10, 31), date(2023, 12, 31)];
            let out = extend_series(&pat, 2, 0, 1);
            let s0: f64 = out[0].raw.parse().unwrap();
            let s1: f64 = out[1].raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(s0, DateStyle::Iso),
                "2024-02-29"
            );
            assert_eq!(
                crate::model::serial_to_date(s1, DateStyle::Iso),
                "2024-04-30"
            );
        }

        {
            let pat = [text("Jan")];
            assert_eq!(raws(&extend_series(&pat, 2, 0, 1)), vec!["Feb", "Mar"]);
            let pat = [text("November"), text("December")];
            assert_eq!(
                raws(&extend_series(&pat, 2, 0, 1)),
                vec!["January", "February"]
            );
            // Weekday names with a step of 2.
            let pat = [text("Mon"), text("Wed")];
            assert_eq!(raws(&extend_series(&pat, 2, 0, 1)), vec!["Fri", "Sun"]);
        }

        {
            // Single "Item 1" increments by one.
            let pat = [text("Item 1")];
            assert_eq!(
                raws(&extend_series(&pat, 2, 0, 1)),
                vec!["Item 2", "Item 3"]
            );
            let pat = [text("Q1"), text("Q3")];
            assert_eq!(raws(&extend_series(&pat, 2, 0, 1)), vec!["Q5", "Q7"]);
            let pat = [text("Row-01")];
            assert_eq!(
                raws(&extend_series(&pat, 2, 0, 1)),
                vec!["Row-02", "Row-03"]
            );
        }

        {
            let pat = [text("Hello")];
            assert_eq!(
                raws(&extend_series(&pat, 3, 0, 1)),
                vec!["Hello", "Hello", "Hello"]
            );
        }

        {
            // A number and a label alternate → the block repeats.
            let pat = [num(5.0), text("x")];
            assert_eq!(
                raws(&extend_series(&pat, 4, 0, 1)),
                vec!["5", "x", "5", "x"]
            );
        }
    }
}
