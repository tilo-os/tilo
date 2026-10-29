use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::model::{FormulaError, a1_to_addr};
use tilo_locale::{Currency, DateStyle};
use ui::tilo_resource::Id;

fn run(cells: &[(&str, &str)]) -> HashMap<Addr, CellValue> {
    let mut raw = HashMap::new();
    for (a, v) in cells {
        raw.insert(a1_to_addr(a).unwrap(), (*v).to_string());
    }
    recalculate(&raw)
}

fn val(map: &HashMap<Addr, CellValue>, a: &str) -> CellValue {
    map.get(&a1_to_addr(a).unwrap())
        .cloned()
        .unwrap_or(CellValue::Empty)
}

fn fmts(pairs: &[(&str, CellFormat)]) -> HashMap<Addr, CellFormat> {
    pairs
        .iter()
        .map(|(a, f)| (a1_to_addr(a).unwrap(), *f))
        .collect()
}

fn sheet(name: &str, cells: &[(&str, &str)]) -> (String, HashMap<Addr, String>) {
    (
        name.to_owned(),
        cells
            .iter()
            .map(|(a, raw)| (a1_to_addr(a).unwrap(), (*raw).to_owned()))
            .collect(),
    )
}

fn in_book(sheets: &[(String, HashMap<Addr, String>)], own: usize) -> HashMap<Addr, CellValue> {
    let inputs: Vec<Inputs> = sheets
        .iter()
        .map(|(_, raw)| Arc::new(raw.clone()))
        .collect();
    let mut calc = Calc::new();
    calc.update(
        sheets
            .iter()
            .zip(&inputs)
            .map(|((name, _), raw)| (name.as_str(), raw)),
    );
    calc.values(own)
}

/// TRIM drops the outer spaces and collapses inner runs to one space.
#[test]
fn trim_collapses_inner_spaces() {
    let m = run(&[
        ("A1", "=TRIM(\"  a   b  \")"),
        ("A2", "=TRIM(\"one\")"),
        ("A3", "=TRIM(\"   \")"),
        ("A4", "=LEN(TRIM(\" x  y  z \"))"),
    ]);
    assert_eq!(val(&m, "A1"), CellValue::Text("a b".into()));
    assert_eq!(val(&m, "A2"), CellValue::Text("one".into()));
    assert_eq!(val(&m, "A3"), CellValue::Text(String::new()));
    assert_eq!(val(&m, "A4"), CellValue::Number(5.0));
}

/// Precedence, references/SUM, IF/comparisons, div-zero/cycles and Excel error codes.
#[test]
fn arithmetic_references_conditionals_and_errors() {
    {
        let m = run(&[("A1", "=2+3*4"), ("A2", "=(2+3)*4"), ("A3", "=2^3^2")]);
        assert_eq!(val(&m, "A1"), CellValue::Number(14.0));
        assert_eq!(val(&m, "A2"), CellValue::Number(20.0));
        // `^` is left-associative, as in Excel: (2^3)^2 = 64
        assert_eq!(val(&m, "A3"), CellValue::Number(64.0));
    }

    {
        let m = run(&[
            ("A1", "10"),
            ("A2", "20"),
            ("A3", "30"),
            ("B1", "=SUM(A1:A3)"),
            ("B2", "=AVERAGE(A1:A3)"),
            ("B3", "=A1+A2"),
        ]);
        assert_eq!(val(&m, "B1"), CellValue::Number(60.0));
        assert_eq!(val(&m, "B2"), CellValue::Number(20.0));
        assert_eq!(val(&m, "B3"), CellValue::Number(30.0));
    }

    {
        let m = run(&[
            ("A1", "5"),
            ("B1", "=IF(A1>3,\"big\",\"small\")"),
            ("C1", "=IF(A1>10,1,0)"),
        ]);
        assert_eq!(val(&m, "B1"), CellValue::Text("big".into()));
        assert_eq!(val(&m, "C1"), CellValue::Number(0.0));
    }

    {
        let m = run(&[("A1", "=1/0"), ("B1", "=B2"), ("B2", "=B1")]);
        assert_eq!(val(&m, "A1"), CellValue::Error(FormulaError::Div0));
        assert!(val(&m, "B1").is_error());
    }

    {
        use crate::model::{CellFormat, format_value};
        // Each formula triggers one specific cause. Assert both the typed
        // error kind *and* the exact canonical string the grid renders
        // (through the real `recalculate` + `format_value` path).
        let m = run(&[
            ("A1", "=1/0"),                // division by zero        → #DIV/0!
            ("A2", "=FOOBAR()"),           // unknown function        → #NAME?
            ("A3", "=NOTACELL"),           // unrecognized bare name  → #NAME?
            ("A4", "=VLOOKUP(1,B1:B3,5)"), // column outside table    → #REF!
            ("A5", "=\"a\"+1"),            // text where number wanted → #VALUE!
            ("A6", "=MATCH(999,B1:B3,0)"), // lookup with no match    → #N/A
            ("B1", "10"),
            ("B2", "20"),
            ("B3", "30"),
        ]);
        let cases = [
            ("A1", FormulaError::Div0, "#DIV/0!"),
            ("A2", FormulaError::Name, "#NAME?"),
            ("A3", FormulaError::Name, "#NAME?"),
            ("A4", FormulaError::Ref, "#REF!"),
            ("A5", FormulaError::Value, "#VALUE!"),
            ("A6", FormulaError::Na, "#N/A"),
        ];
        for (cell, kind, text) in cases {
            let v = val(&m, cell);
            assert_eq!(v, CellValue::Error(kind), "{cell} error kind");
            assert_eq!(
                format_value(&v, CellFormat::General),
                text,
                "{cell} rendered as canonical Excel code",
            );
        }
    }
}

#[test]
fn text_functions_and_format_codes() {
    {
        let m = run(&[
            ("A1", "=UPPER(\"hi\")"),
            ("A2", "=LEFT(\"hello\",2)"),
            ("A3", "=CONCAT(\"a\",\"b\",\"c\")"),
            ("A4", "=LEN(\"abcd\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("HI".into()));
        assert_eq!(val(&m, "A2"), CellValue::Text("he".into()));
        assert_eq!(val(&m, "A3"), CellValue::Text("abc".into()));
        assert_eq!(val(&m, "A4"), CellValue::Number(4.0));
    }

    {
        let m = run(&[
            ("A1", "=SUBSTITUTE(\"a-b-c\",\"-\",\"+\")"),
            ("A2", "=PROPER(\"hello world\")"),
            ("A3", "=FIND(\"b\",\"abc\")"),
            ("A4", "=SEARCH(\"B\",\"abc\")"),
            ("A5", "=REPT(\"ab\",3)"),
            ("A6", "=TEXTJOIN(\",\",TRUE,\"x\",\"y\",\"z\")"),
            ("A7", "=REPLACE(\"abcdef\",2,3,\"XY\")"),
            ("A8", "=VALUE(\"1,234\")"),
            ("A9", "=EXACT(\"Ab\",\"ab\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("a+b+c".into()));
        assert_eq!(val(&m, "A2"), CellValue::Text("Hello World".into()));
        assert_eq!(val(&m, "A3"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A4"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A5"), CellValue::Text("ababab".into()));
        assert_eq!(val(&m, "A6"), CellValue::Text("x,y,z".into()));
        assert_eq!(val(&m, "A7"), CellValue::Text("aXYef".into()));
        assert_eq!(val(&m, "A8"), CellValue::Number(1234.0));
        assert_eq!(val(&m, "A9"), CellValue::Bool(false));
    }

    {
        let serial = crate::model::date_to_serial(2023, 3, 5);
        let m = run(&[
            ("A1", "=TEXT(1234.5,\"#,##0.00\")"),
            ("A2", "=TEXT(0.1234,\"0.0%\")"),
            ("B1", &serial.to_string()),
            ("A3", "=TEXT(B1,\"yyyy-mm-dd\")"),
            ("A4", "=TEXT(B1,\"d mmm yyyy\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("1,234.50".into()));
        assert_eq!(val(&m, "A2"), CellValue::Text("12.3%".into()));
        assert_eq!(val(&m, "A3"), CellValue::Text("2023-03-05".into()));
        assert_eq!(val(&m, "A4"), CellValue::Text("5 Mar 2023".into()));
    }
}

#[test]
#[allow(
    clippy::approx_constant,
    reason = "the literals are spreadsheet inputs and expected outputs, not uses of pi or e"
)]
fn math_statistics_and_logical_functions() {
    {
        let m = run(&[
            ("A1", "5"),
            ("A2", "15"),
            ("A3", "25"),
            ("B1", "=COUNTIF(A1:A3,\">10\")"),
            ("B2", "=SUMIF(A1:A3,\">=15\")"),
        ]);
        assert_eq!(val(&m, "B1"), CellValue::Number(2.0));
        assert_eq!(val(&m, "B2"), CellValue::Number(40.0));
    }

    // Case `math_functions`.
    // ROUND(PI(),2) is expected to be 3.14; that is the assertion, not an
    // approximation of the constant.
    {
        let m = run(&[
            ("A1", "=POWER(2,10)"),
            ("A2", "=MOD(17,5)"),
            ("A3", "=SQRT(144)"),
            ("A4", "=ROUND(3.14159,2)"),
            ("A5", "=ABS(-8)"),
            ("A6", "=INT(7.9)"),
            ("A7", "=PRODUCT(2,3,4)"),
            ("A8", "=TRUNC(9.99)"),
            ("A9", "=LOG(1000)"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Number(1024.0));
        assert_eq!(val(&m, "A2"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A3"), CellValue::Number(12.0));
        assert_eq!(val(&m, "A4"), CellValue::Number(3.14));
        // Halves round away from zero at the digits typed, as in Excel,
        // although 0.285 and 1.005 sit just below their halves in binary.
        for (formula, rounded) in [
            ("=ROUND(0.285,2)", 0.29),
            ("=ROUND(1.005,2)", 1.01),
            ("=ROUND(-1.005,2)", -1.01),
            ("=ROUND(1234.5,-2)", 1200.0),
            ("=ROUNDUP(0.1+0.2,1)", 0.3),
            ("=ROUNDDOWN(4.35*100,0)", 435.0),
        ] {
            let m = run(&[("A1", formula)]);
            assert_eq!(val(&m, "A1"), CellValue::Number(rounded), "{formula}");
        }
        assert_eq!(val(&m, "A5"), CellValue::Number(8.0));
        assert_eq!(val(&m, "A6"), CellValue::Number(7.0));
        assert_eq!(val(&m, "A7"), CellValue::Number(24.0));
        assert_eq!(val(&m, "A8"), CellValue::Number(9.0));
        // LOG(1000) is 3 up to floating-point rounding.
        match val(&m, "A9") {
            CellValue::Number(n) => assert!((n - 3.0).abs() < 1e-9, "LOG(1000)={n}"),
            other => panic!("expected a number, got {other:?}"),
        }
    }

    {
        let m = run(&[
            ("A1", "10"),
            ("A2", "20"),
            ("A3", "30"),
            ("A4", "40"),
            ("B1", "x"),
            ("B2", "y"),
            ("B3", "x"),
            ("B4", "y"),
            ("C1", "=AVERAGEIF(A1:A4,\">15\")"),
            ("C2", "=SUMIFS(A1:A4,B1:B4,\"x\")"),
            ("C3", "=COUNTIFS(A1:A4,\">=20\",B1:B4,\"y\")"),
            ("C4", "=AVERAGEIFS(A1:A4,B1:B4,\"y\")"),
        ]);
        // Average of 20,30,40 = 30.
        assert_eq!(val(&m, "C1"), CellValue::Number(30.0));
        // Sum of A where B="x": 10 + 30 = 40.
        assert_eq!(val(&m, "C2"), CellValue::Number(40.0));
        // A>=20 AND B="y": A2(20,y) and A4(40,y) → 2.
        assert_eq!(val(&m, "C3"), CellValue::Number(2.0));
        // Average of A where B="y": (20+40)/2 = 30.
        assert_eq!(val(&m, "C4"), CellValue::Number(30.0));
    }

    {
        let m = run(&[
            ("A1", "=IFERROR(1/0,\"safe\")"),
            ("A2", "=IFS(FALSE,1,TRUE,2)"),
            ("A3", "=XOR(TRUE,TRUE,TRUE)"),
            ("A4", "=SWITCH(3,1,\"a\",3,\"c\",\"z\")"),
            ("A5", "=IF(AND(1>0,2>1),\"yes\",\"no\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("safe".into()));
        assert_eq!(val(&m, "A2"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A3"), CellValue::Bool(true));
        assert_eq!(val(&m, "A4"), CellValue::Text("c".into()));
        assert_eq!(val(&m, "A5"), CellValue::Text("yes".into()));
    }
}

#[test]
fn lookup_and_date_time_functions() {
    {
        // A small table: A=key, B=payload.
        let m = run(&[
            ("A1", "1"),
            ("B1", "one"),
            ("A2", "2"),
            ("B2", "two"),
            ("A3", "3"),
            ("B3", "three"),
            ("D1", "=VLOOKUP(2,A1:B3,2,FALSE)"),
            ("D2", "=INDEX(B1:B3,3)"),
            ("D3", "=MATCH(3,A1:A3,0)"),
            ("D4", "=HLOOKUP(2,A1:C1,1,FALSE)"),
            ("C1", "hit"),
            ("D5", "=CHOOSE(2,\"a\",\"b\",\"c\")"),
        ]);
        assert_eq!(val(&m, "D1"), CellValue::Text("two".into()));
        assert_eq!(val(&m, "D2"), CellValue::Text("three".into()));
        assert_eq!(val(&m, "D3"), CellValue::Number(3.0));
        assert_eq!(val(&m, "D5"), CellValue::Text("b".into()));
    }

    {
        let m = run(&[
            ("A1", "=DATE(2023,3,15)"),
            ("A2", "=YEAR(A1)"),
            ("A3", "=MONTH(A1)"),
            ("A4", "=DAY(A1)"),
            ("A5", "=WEEKDAY(A1)"),
            ("A6", "=HOUR(TIME(14,5,0))"),
            ("A7", "=MINUTE(TIME(14,5,0))"),
        ]);
        // The serial must round-trip through the model helpers.
        let serial = crate::model::date_to_serial(2023, 3, 15);
        assert_eq!(val(&m, "A1"), CellValue::Number(serial));
        assert_eq!(val(&m, "A2"), CellValue::Number(2023.0));
        assert_eq!(val(&m, "A3"), CellValue::Number(3.0));
        assert_eq!(val(&m, "A4"), CellValue::Number(15.0));
        // 2023-03-15 is a Wednesday → 4 in Excel's Sunday-first numbering.
        assert_eq!(val(&m, "A5"), CellValue::Number(4.0));
        assert_eq!(val(&m, "A6"), CellValue::Number(14.0));
        assert_eq!(val(&m, "A7"), CellValue::Number(5.0));
    }

    {
        let m = run(&[("A1", "=TODAY()"), ("A2", "=NOW()")]);
        // A serial well past 2020-01-01 (43831) and before 2100.
        match val(&m, "A1") {
            CellValue::Number(n) => assert!(n > 43831.0 && n < 73415.0, "today serial {n}"),
            other => panic!("TODAY must be a number, got {other:?}"),
        }
        match val(&m, "A2") {
            CellValue::Number(n) => assert!(n > 43831.0, "now serial {n}"),
            other => panic!("NOW must be a number, got {other:?}"),
        }
    }
}

/// Currency, percent, duration and unitless result formats.
#[test]
fn result_formats_propagate_through_arithmetic() {
    {
        // =5*B2 where B2 is $100 must render as currency.
        let f = fmts(&[("B2", CellFormat::Currency(Currency::USD))]);
        assert_eq!(
            infer_result_format("5*B2", &f),
            Some(CellFormat::Currency(Currency::USD))
        );
        // order doesn't matter
        assert_eq!(
            infer_result_format("B2*5", &f),
            Some(CellFormat::Currency(Currency::USD))
        );
        // division by a scalar keeps the currency
        assert_eq!(
            infer_result_format("B2/2", &f),
            Some(CellFormat::Currency(Currency::USD))
        );
    }

    {
        let f = fmts(&[
            ("A1", CellFormat::Currency(Currency::EUR)),
            ("A2", CellFormat::Currency(Currency::EUR)),
        ]);
        assert_eq!(
            infer_result_format("A1+A2", &f),
            Some(CellFormat::Currency(Currency::EUR))
        );
    }

    {
        let f = fmts(&[
            ("A1", CellFormat::Currency(Currency::USD)),
            ("A2", CellFormat::Currency(Currency::EUR)),
        ]);
        // USD + EUR is unclear → plain number (None).
        assert_eq!(infer_result_format("A1+A2", &f), None);
        assert_eq!(infer_result_format("A1*A2", &f), None);
    }

    {
        let f = fmts(&[("A1", CellFormat::Percent), ("A2", CellFormat::Percent)]);
        assert_eq!(infer_result_format("A1+A2", &f), Some(CellFormat::Percent));
        assert_eq!(infer_result_format("A1*2", &f), Some(CellFormat::Percent));
    }

    {
        let f = fmts(&[
            ("B2", CellFormat::Currency(Currency::GBP)),
            ("B3", CellFormat::Currency(Currency::GBP)),
            ("B4", CellFormat::Currency(Currency::GBP)),
        ]);
        assert_eq!(
            infer_result_format("SUM(B2:B4)", &f),
            Some(CellFormat::Currency(Currency::GBP))
        );
        assert_eq!(
            infer_result_format("AVERAGE(B2:B4)", &f),
            Some(CellFormat::Currency(Currency::GBP))
        );
    }

    {
        let f = fmts(&[
            ("A1", CellFormat::Date(DateStyle::Iso)),
            ("A2", CellFormat::Date(DateStyle::Iso)),
        ]);
        // Difference of two dates is a number of days → plain (None).
        assert_eq!(infer_result_format("A1-A2", &f), None);
        assert_eq!(
            infer_result_format("A1+7", &f),
            Some(CellFormat::Date(DateStyle::Iso))
        );
    }

    {
        let f = fmts(&[]);
        assert_eq!(infer_result_format("2+3*4", &f), None);
    }

    {
        let f = fmts(&[("A1", CellFormat::Currency(Currency::USD))]);
        assert_eq!(infer_result_format("A1>100", &f), None);
        assert_eq!(infer_result_format("A1&\"x\"", &f), None);
    }

    {
        // (E2-B2)/B2 where E2,B2 are currency → a ratio, which is unitless.
        let f = fmts(&[
            ("E2", CellFormat::Currency(Currency::USD)),
            ("B2", CellFormat::Currency(Currency::USD)),
        ]);
        // (currency - currency) = currency; currency / currency = Mixed→None.
        assert_eq!(infer_result_format("(E2-B2)/B2", &f), None);
    }
}

/// Formulas evaluate as Excel evaluates them.
#[test]
fn formulas_follow_excel() {
    let data = [
        ("A1", "10"),
        ("A2", "20"),
        ("A3", "30"),
        ("B1", "apple"),
        ("B2", "banana"),
        ("B3", "cherry"),
    ];
    let num = |n: f64| CellValue::Number(n);
    let text = |t: &str| CellValue::Text(t.into());
    let err = CellValue::Error;
    let serial = crate::model::date_to_serial(2023, 3, 15);
    for (formula, want) in [
        ("=-2^2", num(4.0)),
        ("=ISNA(NA())", CellValue::Bool(true)),
        ("=ISERR(NA())", CellValue::Bool(false)),
        ("=IFNA(NA(),\"n\")", text("n")),
        ("=COUNTIF(B1:B3,\"b*\")", num(1.0)),
        ("=COUNTIF(B1:B3,\"?pple\")", num(1.0)),
        ("=VLOOKUP(\"b*\",B1:B3,1,FALSE)", text("banana")),
        ("=MATCH(\"ch*\",B1:B3,0)", num(3.0)),
        ("=SEARCH(\"b*\",\"abc\")", num(2.0)),
        ("=SUM(A:A)", num(60.0)),
        (
            format!("=TEXT({serial},\"dddd, mmmm d\")").as_str(),
            text("Wednesday, March 15"),
        ),
        ("=TEXT(-5,\"0;(0)\")", text("(5)")),
        ("=\"a\"<\"B\"", CellValue::Bool(true)),
        ("=\"abc\"=\"ABC\"", CellValue::Bool(true)),
        ("=TRUE>1", CellValue::Bool(true)),
        ("=SUM(A1,\"5\")", num(15.0)),
        ("=DAY(-1)", err(FormulaError::Num)),
        ("=INDEX(A1:B3,0,1)", num(10.0)),
        ("=ROUND(2.5)", err(FormulaError::Error)),
        ("=AND(\"x\")", err(FormulaError::Value)),
        ("=IF(\"x\",1,2)", err(FormulaError::Value)),
        ("=LEFT(\"abc\",-1)", err(FormulaError::Value)),
        ("=REPT(\"a\",-1)", err(FormulaError::Value)),
        ("=CHAR(0)", err(FormulaError::Value)),
        ("=1/0", err(FormulaError::Div0)),
        ("=ZZZ(1)", err(FormulaError::Name)),
    ] {
        let mut cells = data.to_vec();
        cells.push(("H1", formula));
        assert_eq!(val(&run(&cells), "H1"), want, "{formula}");
    }
}

/// A formula reading itself, directly or round a loop, shows `#CYCLE!`.
#[test]
fn a_cycle_shows_cycle() {
    let m = run(&[
        ("A1", "=A1"),
        ("B1", "=C1+1"),
        ("C1", "=B1"),
        ("D1", "=B1*0"),
    ]);
    for cell in ["A1", "B1", "C1"] {
        assert_eq!(
            val(&m, cell),
            CellValue::Error(FormulaError::Cycle),
            "{cell}"
        );
    }
    assert!(val(&m, "D1").is_error(), "reading a cycle is an error too");
}

/// Typed input reaches the engine as the value the app read it as, and is
/// never read a second time: text stays text, however it looks.
#[test]
fn literals_reach_the_engine_as_read() {
    let m = run(&[
        ("A1", "1000"),
        ("A2", "TRUE"),
        ("A3", "false"),
        ("A4", "#N/A"),
        ("A5", "$1,000"),
        ("A6", "50%"),
        ("A7", "  "),
        ("B1", "=A1*2"),
        ("B2", "=A2+0"),
        ("B3", "=ISTEXT(A4)"),
        ("B4", "=ISTEXT(A5)"),
        ("B5", "=A6*2"),
        ("B6", "=ISBLANK(A7)"),
    ]);
    assert_eq!(val(&m, "A3"), CellValue::Bool(false));
    assert_eq!(val(&m, "A4"), CellValue::Text("#N/A".into()));
    assert_eq!(val(&m, "B1"), CellValue::Number(2000.0));
    assert_eq!(val(&m, "B2"), CellValue::Number(1.0));
    assert_eq!(val(&m, "B3"), CellValue::Bool(true));
    assert_eq!(
        val(&m, "B4"),
        CellValue::Bool(true),
        "text is never read again as money"
    );
    assert_eq!(val(&m, "B5"), CellValue::Number(1.0));
    assert_eq!(val(&m, "A7"), CellValue::Empty);
    assert_eq!(val(&m, "B6"), CellValue::Bool(true));
}

/// A formula reads cells and ranges of other sheets, by any spelling of
/// their names; a sheet the workbook lacks is `#REF!`.
#[test]
fn a_formula_reads_cells_and_ranges_of_another_sheet() {
    let book = vec![
        sheet(
            "Summary",
            &[
                ("A1", "=Rates!B1*2"),
                ("B1", "=SUM('Q3 data'!A1:A3)"),
                ("C1", "=VLOOKUP(\"b\",rates!A1:B2,2,FALSE)"),
                ("D1", "=Nowhere!A1"),
                ("E1", "='P&L: 1/2'!A1"),
                ("F1", "=SUM(rates!B:B)"),
            ],
        ),
        sheet(
            "Rates",
            &[("A1", "a"), ("B1", "21"), ("A2", "b"), ("B2", "5")],
        ),
        sheet("Q3 data", &[("A1", "1"), ("A2", "2"), ("A3", "=A1+A2")]),
        sheet("P&L: 1/2", &[("A1", "7")]),
        sheet("rates", &[("B1", "1000")]),
    ];
    let values = in_book(&book, 0);
    let at = |a: &str| values.get(&a1_to_addr(a).unwrap()).cloned();
    assert_eq!(at("A1"), Some(CellValue::Number(42.0)));
    assert_eq!(at("B1"), Some(CellValue::Number(6.0)));
    assert_eq!(at("C1"), Some(CellValue::Number(5.0)));
    assert_eq!(at("D1"), Some(CellValue::Error(FormulaError::Ref)));
    assert_eq!(
        at("E1"),
        Some(CellValue::Number(7.0)),
        "any name a sheet may have"
    );
    assert_eq!(
        at("F1"),
        Some(CellValue::Number(26.0)),
        "the first of two sheets with one name is the one references reach"
    );
}

#[test]
fn a_cycle_across_sheets_is_found() {
    let values = in_book(
        &[
            sheet("One", &[("A1", "=Two!A1+1")]),
            sheet("Two", &[("A1", "=One!A1+1")]),
        ],
        0,
    );
    assert_eq!(
        values.get(&(0, 0)),
        Some(&CellValue::Error(FormulaError::Cycle))
    );
}

/// The calculator keeps its workbook: each update writes what changed,
/// clears what went, and the values follow.
#[test]
fn updates_rewrite_only_what_changed() {
    let mut calc = Calc::new();
    let (_, mut raw) = sheet(
        "S",
        &[
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "=SUM(A1:A2)"),
            ("B1", "=A1:A2*10"),
        ],
    );
    calc.update([("S", &Arc::new(raw.clone()))]);
    let at = |calc: &Calc, a: &str| calc.values(0).get(&a1_to_addr(a).unwrap()).cloned();
    assert_eq!(at(&calc, "A3"), Some(CellValue::Number(3.0)));
    assert_eq!(
        at(&calc, "B2"),
        Some(CellValue::Number(20.0)),
        "an array spills"
    );

    raw.insert(a1_to_addr("A2").unwrap(), "40".into());
    calc.update([("S", &Arc::new(raw.clone()))]);
    assert_eq!(at(&calc, "A3"), Some(CellValue::Number(41.0)));
    assert_eq!(at(&calc, "B2"), Some(CellValue::Number(400.0)));

    raw.insert(a1_to_addr("B2").unwrap(), "x".into());
    calc.update([("S", &Arc::new(raw.clone()))]);
    assert_eq!(at(&calc, "B1"), Some(CellValue::Error(FormulaError::Spill)));
    assert_eq!(at(&calc, "B2"), Some(CellValue::Text("x".into())));

    raw.remove(&a1_to_addr("B2").unwrap());
    raw.remove(&a1_to_addr("A1").unwrap());
    calc.update([("S", &Arc::new(raw.clone()))]);
    assert_eq!(at(&calc, "A1"), None);
    assert_eq!(at(&calc, "A3"), Some(CellValue::Number(40.0)));
    assert_eq!(
        at(&calc, "B2"),
        Some(CellValue::Number(400.0)),
        "spills again"
    );

    // A renamed sheet: the workbook is built anew under the new name.
    let other = sheet("T", &[("A1", "=S!A3")]).1;
    let (raw, other) = (Arc::new(raw), Arc::new(other));
    calc.update([("S2", &raw), ("T", &other)]);
    assert_eq!(
        calc.values(1).get(&(0, 0)),
        Some(&CellValue::Error(FormulaError::Ref))
    );
    calc.update([("S", &raw), ("T", &other)]);
    assert_eq!(calc.values(1).get(&(0, 0)), Some(&CellValue::Number(40.0)));
}

/// A column of formulas each reading the one above (a running total) is as
/// deep as the sheet is long, and evaluates without exhausting the stack.
#[test]
fn a_long_chain_evaluates() {
    const ROWS: u32 = 50_000;
    let mut raw = HashMap::new();
    raw.insert((0, 0), "1".to_owned());
    for row in 1..ROWS {
        raw.insert((0, row), format!("=A{row}+1"));
    }
    let values = recalculate(&raw);
    assert_eq!(
        values.get(&(0, ROWS - 1)),
        Some(&CellValue::Number(f64::from(ROWS)))
    );
}

/// `parse_error` names what does not parse, with the error the cell shows,
/// and passes formulas that parse and input that is no formula; the cell
/// shows the error rather than a repaired formula's value.
#[test]
fn parse_errors_say_what_is_wrong() {
    let problem = |raw: &str| parse_error(raw).map(|e| (e.error.text(), e.problem));
    assert_eq!(problem("=SUM(A1:A3)"), None);
    assert_eq!(problem("=Rates!A2"), None);
    assert_eq!(
        problem("=TOTAL"),
        None,
        "an unknown name evaluates to #NAME?"
    );
    assert_eq!(problem("plain (text"), None);
    assert_eq!(problem("=\")\"&'a)'!A1"), None, "quoted parentheses");
    for (raw, why) in [
        ("=SUM(A1", "A parenthesis is opened but not closed"),
        ("=1+2)", "A closing parenthesis has no matching opening one"),
        (
            "=",
            "The formula is empty: type a value or reference after =",
        ),
        (
            "=1+",
            "The formula ends with an operator: add a value or reference after it",
        ),
        ("=\"open", "A text value is missing its closing quote"),
        (
            "=1 2",
            "The formula could not be read: check for a missing value, operator or parenthesis",
        ),
    ] {
        assert_eq!(problem(raw), Some(("#ERROR!", why.to_owned())), "{raw}");
    }
    let m = run(&[("A1", "2"), ("B1", "=SUM(A1"), ("C1", "=")]);
    assert_eq!(val(&m, "B1"), CellValue::Error(FormulaError::Error));
    assert_eq!(val(&m, "C1"), CellValue::Error(FormulaError::Error));
}

/// Formulas filled down share the parse of the one above; each still reads
/// its own row, and one that only looks like a copy reads what it names.
#[test]
fn formulas_filled_down_read_their_own_rows() {
    let m = run(&[
        ("A1", "1"),
        ("A2", "2"),
        ("A3", "3"),
        ("B1", "=A1*10+$A$1"),
        ("B2", "=A2*10+$A$1"),
        ("B3", "=A3*10+$A$1"),
        ("C2", "=SUM(1:1)"),
        ("C3", "=SUM(1:1)"),
        ("D2", "=A1"),
        ("D3", "=A1"),
        ("E1", "=SUM(A$1:A1)"),
        ("E2", "=SUM(A$1:A2)"),
        ("E3", "=SUM(A$1:A3)"),
    ]);
    assert_eq!(val(&m, "B2"), CellValue::Number(21.0));
    assert_eq!(val(&m, "B3"), CellValue::Number(31.0));
    assert_eq!(val(&m, "C2"), CellValue::Number(13.0));
    assert_eq!(
        val(&m, "C3"),
        CellValue::Number(13.0),
        "whole rows as written"
    );
    assert_eq!(val(&m, "D3"), CellValue::Number(1.0), "no copy: as written");
    assert_eq!(val(&m, "E3"), CellValue::Number(6.0));
}

/// Every sheet evaluates in one workbook kept across states: showing
/// another sheet reads what is computed, and a change re-reads only the
/// sheet it touched.
#[test]
fn a_workbook_calc_rereads_only_the_sheets_a_change_touched() {
    use crate::resource::CellWrite;
    use crate::workbook::Workbook;
    use ui::tilo_resource::Reader;

    let mut book = crate::sample::replica();
    let mut reader = Reader::<Workbook>::read(book.doc()).unwrap();
    let budget = Id::from_raw(crate::sample::BUDGET_SHEET_ID);
    let sales = Id::from_raw(crate::sample::SALES_SHEET_ID);
    let mut calc = WorkbookCalc::default();
    calc.update(reader.state(), None);
    let sales_inputs = Arc::clone(&calc.inputs[&sales]);
    let sales_values = calc.values(&sales);
    assert!(!sales_values.is_empty());

    book.apply(
        Workbook::WRITE_CELLS.with(crate::resource::verbs::WriteCells {
            sheet: budget.clone(),
            cells: vec![CellWrite {
                cell: "Z1".to_owned(),
                input: Some("=1+2".to_owned()),
                ..CellWrite::default()
            }],
        }),
    )
    .unwrap();
    assert!(reader.sync(book.doc()).unwrap());
    calc.update(reader.state(), None);
    assert_eq!(
        calc.values(&budget).get(&a1_to_addr("Z1").unwrap()),
        Some(&CellValue::Number(3.0))
    );
    assert!(
        Arc::ptr_eq(&calc.inputs[&sales], &sales_inputs),
        "the untouched sheet is not read again"
    );
    assert_eq!(calc.values(&sales), sales_values);
}

/// Timings at the size of a large sheet, to be read in a release build:
/// `cargo test --release -p sheets --lib -- --ignored --nocapture
/// large_workbook_timings`. Opening builds the workbook; showing another
/// sheet must cost no evaluation; an edit writes one cell and evaluates.
#[test]
#[ignore = "a timing run rather than a check; run it in a release build"]
fn large_workbook_timings() {
    use std::time::Instant;

    // A number column and three formula columns filled down: `D` adds two
    // cells of its row, or, chained, the running total above it.
    let sheet = |rows: u32, chain: bool| -> HashMap<Addr, String> {
        let mut raw = HashMap::new();
        for r in 1..=rows {
            raw.insert((0, r - 1), r.to_string());
            raw.insert((1, r - 1), format!("=A{r}*2"));
            raw.insert((2, r - 1), format!("=B{r}+1"));
            let total = match (chain, r) {
                (true, 1) => "=B1".to_owned(),
                (true, _) => format!("=D{}+B{r}", r - 1),
                (false, _) => format!("=A{r}+C{r}"),
            };
            raw.insert((3, r - 1), total);
        }
        raw
    };
    for chain in [false, true] {
        for rows in [10_000u32, 100_000] {
            let big = Arc::new(sheet(rows, chain));
            let small: Inputs = Arc::new(HashMap::from([((0, 0), format!("=Big!D{rows}"))]));
            let mut calc = Calc::new();

            let at = Instant::now();
            calc.update([("Big", &big), ("Small", &small)]);
            let open = at.elapsed();

            let at = Instant::now();
            calc.update([("Big", &big), ("Small", &small)]);
            let small_values = calc.values(1);
            calc.update([("Big", &big), ("Small", &small)]);
            let big_values = calc.values(0);
            let switch = at.elapsed();

            let mut edited = (*big).clone();
            edited.insert((0, 0), "5".to_owned());
            let edited = Arc::new(edited);
            let at = Instant::now();
            calc.update([("Big", &edited), ("Small", &small)]);
            let edit = at.elapsed();

            let at = Instant::now();
            let values = calc.values(0);
            let read = at.elapsed();

            let money: HashMap<Addr, CellFormat> = (0..rows)
                .map(|row| ((0, row), CellFormat::Currency(Currency::USD)))
                .collect();
            let at = Instant::now();
            let inferred = big
                .values()
                .filter_map(|raw| infer_result_format(raw.strip_prefix('=')?, &money))
                .count();
            let formats = at.elapsed();

            eprintln!(
                "chain={chain} rows={rows} open={open:?} switch={switch:?} edit={edit:?} \
                 values={read:?} formats={formats:?}"
            );
            let last = (3, rows - 1);
            assert_eq!(small_values.get(&(0, 0)), big_values.get(&last));
            assert_ne!(
                values.get(&(3, 0)),
                big_values.get(&(3, 0)),
                "the edit is evaluated"
            );
            std::hint::black_box(inferred);
        }
    }
}
