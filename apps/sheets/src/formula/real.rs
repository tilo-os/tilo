//! Results that must stay in the real numbers. NaN and the infinities are
//! not spreadsheet values: maths with no real result is an error value, with
//! the code Excel gives, so IFERROR and ISERROR can catch it and the cell
//! never shows a NaN or an infinity sign.

use crate::model::{CellValue, FormulaError};

/// A computed number as a cell value: NaN and the infinities are not
/// spreadsheet numbers, so they become `#NUM!`.
pub(super) fn finite_number(n: f64) -> CellValue {
    if n.is_finite() {
        CellValue::Number(n)
    } else {
        CellValue::Error(FormulaError::Num)
    }
}

/// `a ^ b` and `POWER(a, b)` as Excel computes them: zero to a negative power
/// divides by zero, `0 ^ 0` is undefined, and a negative base to a fractional
/// power (`POWER(-8, 1/3)`) has no real result.
pub(super) fn power(a: f64, b: f64) -> CellValue {
    if a == 0.0 && b < 0.0 {
        CellValue::Error(FormulaError::Div0)
    } else if a == 0.0 && b == 0.0 {
        CellValue::Error(FormulaError::Num)
    } else {
        finite_number(a.powf(b))
    }
}

/// `LOG(n, base)` as Excel computes it: a base of 1 divides by `ln 1 = 0`,
/// and a non-positive number or base has no logarithm.
pub(super) fn logarithm(n: f64, base: f64) -> CellValue {
    if base.ln() == 0.0 {
        CellValue::Error(FormulaError::Div0)
    } else if n <= 0.0 || base <= 0.0 {
        CellValue::Error(FormulaError::Num)
    } else {
        finite_number(n.log(base))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::recalculate;
    use crate::model::{Addr, CellValue, FormulaError, a1_to_addr};

    fn run(cells: &[(&str, &str)]) -> HashMap<Addr, CellValue> {
        let raw: HashMap<Addr, String> = cells
            .iter()
            .map(|(a, v)| (a1_to_addr(a).expect("a cell address"), (*v).to_owned()))
            .collect();
        recalculate(&raw)
    }

    fn val(map: &HashMap<Addr, CellValue>, a: &str) -> CellValue {
        map.get(&a1_to_addr(a).expect("a cell address"))
            .cloned()
            .unwrap_or(CellValue::Empty)
    }

    /// Maths with no real result is an error value IFERROR and ISERROR see,
    /// with the code Excel gives, never a NaN or an infinity in the cell.
    #[test]
    fn out_of_range_maths_is_an_error_value() {
        use crate::model::{CellFormat, format_value};
        let cases = [
            ("=SQRT(-1)", FormulaError::Num),
            ("=LN(0)", FormulaError::Num),
            ("=LN(-1)", FormulaError::Num),
            ("=LOG10(0)", FormulaError::Num),
            ("=LOG(-10)", FormulaError::Num),
            ("=LOG(8,0)", FormulaError::Num),
            ("=LOG(8,1)", FormulaError::Div0),
            ("=LOG(8,1/0)", FormulaError::Div0),
            ("=EXP(1000)", FormulaError::Num),
            ("=POWER(-8,1/3)", FormulaError::Num),
            ("=(-8)^(1/3)", FormulaError::Num),
            ("=POWER(0,-1)", FormulaError::Div0),
            ("=0^-1", FormulaError::Div0),
            ("=POWER(0,0)", FormulaError::Num),
            ("=10^400", FormulaError::Num),
            ("=1E300*1E300", FormulaError::Num),
            ("=SUM(1E308,1E308)", FormulaError::Num),
        ];
        for (formula, kind) in cases {
            let m = run(&[("A1", formula)]);
            let v = val(&m, "A1");
            assert_eq!(v, CellValue::Error(kind), "{formula}");
            assert_eq!(
                format_value(&v, CellFormat::General),
                kind.text(),
                "{formula}"
            );
        }

        let m = run(&[
            ("A1", "=IFERROR(SQRT(-1),\"x\")"),
            ("A2", "=ISERROR(LN(0))"),
            ("A3", "=ISERROR(SQRT(4))"),
            ("A4", "=ISERR(LN(0))"),
            ("A5", "=ISERR(MATCH(9,B1:B2,0))"),
            ("A6", "=ISNA(MATCH(9,B1:B2,0))"),
            ("A7", "=ISNA(1/0)"),
            ("A8", "=POWER(-8,3)"),
            ("A9", "=LOG(8,2)"),
            ("B1", "1"),
            ("B2", "2"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("x".into()));
        assert_eq!(val(&m, "A2"), CellValue::Bool(true));
        assert_eq!(val(&m, "A3"), CellValue::Bool(false));
        assert_eq!(val(&m, "A4"), CellValue::Bool(true));
        assert_eq!(val(&m, "A5"), CellValue::Bool(false));
        assert_eq!(val(&m, "A6"), CellValue::Bool(true));
        assert_eq!(val(&m, "A7"), CellValue::Bool(false));
        // A negative base to a whole power is still a number.
        assert_eq!(val(&m, "A8"), CellValue::Number(-512.0));
        match val(&m, "A9") {
            CellValue::Number(n) => assert!((n - 3.0).abs() < 1e-9, "LOG(8,2)={n}"),
            other => panic!("expected a number, got {other:?}"),
        }
    }
}
