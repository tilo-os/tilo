//! Rounding: `ROUND`, `ROUNDUP`, `ROUNDDOWN`, and `FLOOR` and `CEILING` to a
//! multiple. Each works on the decimal digits a spreadsheet keeps rather
//! than on the binary float, so a number rounds as it reads.

use std::hash::BuildHasher;

use super::{Engine, Expr, number_to_int};
use crate::model::{CellValue, FormulaError};

#[derive(Clone, Copy)]
pub(super) enum RoundMode {
    Nearest,
    Down,
    Up,
}

/// `n` times ten to the `places`, moved in decimal at the 15 significant
/// digits a spreadsheet keeps, so the digits a person typed are the digits
/// rounded: `1.005` two places on is `100.5`, where the binary product
/// `1.005 * 100` is `100.49999999999999` and would round to `1.00`.
fn shift_decimal(n: f64, places: i32) -> f64 {
    let repr = format!("{n:.14e}");
    let Some((mantissa, exponent)) = repr.split_once('e') else {
        return n; // infinite or NaN
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return n;
    };
    format!("{mantissa}e{}", exponent.saturating_add(places))
        .parse()
        .unwrap_or(n)
}

impl<S: BuildHasher> Engine<'_, S> {
    pub(super) fn round_fn(&mut self, args: &[Expr], mode: RoundMode) -> CellValue {
        let Some(value_expr) = args.first() else {
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(value_expr);
        if v.is_error() {
            return v;
        }
        let Some(n) = v.as_number() else {
            return CellValue::Error(FormulaError::Value);
        };
        // Past ±308 places no digit is left to round, so clamping there
        // loses nothing and keeps the exponent in range.
        let digits = args.get(1).map_or(0, |a| {
            let scale = self.eval_expr(a).as_number().unwrap_or(0.0);
            i32::try_from(number_to_int(scale).clamp(-308, 308)).unwrap_or(0)
        });
        let scaled = shift_decimal(n, digits);
        if !scaled.is_finite() {
            return CellValue::Number(n);
        }
        let rounded = match mode {
            RoundMode::Nearest => scaled.round(),
            RoundMode::Down => scaled.trunc(),
            RoundMode::Up => {
                if scaled >= 0.0 {
                    scaled.ceil()
                } else {
                    scaled.floor()
                }
            }
        };
        CellValue::Number(shift_decimal(rounded, digits.saturating_neg()))
    }

    /// `FLOOR`/`CEILING(number, [significance])`: `number` rounded by `round`
    /// to a multiple of `significance` (1 when it is left out), as Excel and
    /// Calc do: `FLOOR(7, 5)` is 5, `CEILING(2.3, 0.5)` is 2.5, a negative
    /// number goes the way `round` goes on the number line, and a positive
    /// number with a negative significance is `#NUM!`. A nonzero number with
    /// a significance of 0 is `by_zero`: Excel's `CEILING` gives 0 there and
    /// its `FLOOR` gives `#DIV/0!`, as Calc's does an error.
    pub(super) fn round_to_multiple(
        &mut self,
        args: &[Expr],
        round: fn(f64) -> f64,
        by_zero: CellValue,
    ) -> CellValue {
        let (Some(number), significance) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        if args.len() > 2 {
            return CellValue::Error(FormulaError::Error);
        }
        let mut values = Vec::with_capacity(2);
        for arg in std::iter::once(number).chain(significance) {
            let value = self.eval_expr(arg);
            if value.is_error() {
                return value;
            }
            let Some(n) = value.as_number() else {
                return CellValue::Error(FormulaError::Value);
            };
            values.push(n);
        }
        let (n, step) = match values.as_slice() {
            [n] => (*n, 1.0),
            [n, step] => (*n, *step),
            _ => return CellValue::Error(FormulaError::Error),
        };
        if n == 0.0 {
            return CellValue::Number(0.0);
        }
        if step == 0.0 {
            return by_zero;
        }
        if n > 0.0 && step < 0.0 {
            return CellValue::Error(FormulaError::Num);
        }
        // `0.3 / 0.1` is 2.9999999999999996 in binary; read at 15 digits it
        // is the 3 the person meant, and so is `3 * 0.1`.
        let steps = round(shift_decimal(n / step, 0));
        CellValue::Number(shift_decimal(steps * step, 0))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::recalculate;
    use crate::model::{CellValue, FormulaError, a1_to_addr};

    fn value(formula: &str) -> CellValue {
        let cell = a1_to_addr("A1").expect("A1 is a cell");
        let raw = HashMap::from([(cell, formula.to_owned())]);
        recalculate(&raw).remove(&cell).unwrap_or(CellValue::Empty)
    }

    /// FLOOR and CEILING round to the multiple of their second argument,
    /// which they used to ignore (`FLOOR(7,5)` read 7), and land on the
    /// multiple a decimal step names although binary division misses it.
    #[test]
    fn floor_and_ceiling_round_to_a_multiple_of_the_significance() {
        for (formula, expected) in [
            ("=FLOOR(7,5)", 5.0),
            ("=CEILING(7,5)", 10.0),
            ("=CEILING(2.3,0.5)", 2.5),
            ("=FLOOR(0.3,0.1)", 0.3),
            ("=CEILING(0.3,0.1)", 0.3),
            ("=CEILING(4.42,0.05)", 4.45),
            ("=FLOOR(-2.5,2)", -4.0),
            ("=CEILING(-2.5,2)", -2.0),
            ("=FLOOR(-2.5,-2)", -2.0),
            ("=CEILING(-2.5,-2)", -4.0),
            ("=CEILING(5,0)", 0.0),
            ("=FLOOR(4.7)", 4.0),
            ("=CEILING(4.2)", 5.0),
        ] {
            assert_eq!(value(formula), CellValue::Number(expected), "{formula}");
        }
        assert_eq!(value("=FLOOR(2.5,-2)"), CellValue::Error(FormulaError::Num));
        // Excel has no multiple of 0 for FLOOR to fall to, and Calc agrees.
        assert_eq!(value("=FLOOR(5,0)"), CellValue::Error(FormulaError::Div0));
        assert_eq!(value("=FLOOR(-5,0)"), CellValue::Error(FormulaError::Div0));
        assert_eq!(value("=FLOOR(0,0)"), CellValue::Number(0.0));
        assert_eq!(value("=CEILING(0,0)"), CellValue::Number(0.0));
        assert_eq!(
            value("=CEILING(\"x\",2)"),
            CellValue::Error(FormulaError::Value)
        );
    }
}
