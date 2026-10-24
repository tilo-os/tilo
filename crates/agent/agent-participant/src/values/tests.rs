#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::collections::BTreeMap;

use serde_json::json;
use tilo_actions::{ActionParam, ParamType};

use super::*;

/// A workbook whose `read` answers cells as Sheets does, and a notes app
/// whose `read` answers a page.
#[derive(Default)]
struct Room {
    cells: BTreeMap<String, serde_json::Value>,
    asked: Vec<ViewRef>,
}

impl Reads for Room {
    fn read(&mut self, view: &ViewRef) -> Result<serde_json::Value, String> {
        self.asked.push(view.clone());
        match (view.app.as_str(), view.operation.as_str()) {
            (CELLS_APP, "read") => {
                let item = view.arguments.get("item").cloned().unwrap_or_default();
                self.cells
                    .get(&item)
                    .cloned()
                    .ok_or_else(|| format!("no sheet range `{item}`"))
            }
            ("notes", "read") if view.record.as_deref() == Some("uuid:7") => {
                Ok(json!({"title": "Claim", "fields": {"payout": "$4,250.00", "excess": 500}}))
            }
            _ => Err(format!("no read `{}` of {}", view.operation, view.app)),
        }
    }
}

fn room() -> Room {
    let mut room = Room::default();
    for (item, rows) in [
        ("Sheet1!B4", json!([[1200.5]])),
        ("Sheet1!B2:B4", json!([[400], [300.5], [500]])),
        ("Q3 Budget!C7", json!([["$2,000.00"]])),
        ("Sheet1!C1", json!([["2026-10-23"]])),
        ("Sheet1!D9", json!([[null]])),
    ] {
        room.cells.insert(item.to_owned(), json!({ "rows": rows }));
    }
    room
}

fn bound(
    arguments: serde_json::Value,
    params: &[ActionParam],
) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(mut arguments) = arguments else {
        return Err("arguments are an object".into());
    };
    bind(&mut arguments, params, &mut room())?;
    Ok(serde_json::Value::Object(arguments))
}

#[test]
fn a_whole_reference_is_its_typed_value_and_inside_text_it_is_written_out() {
    let out = bound(
        json!({
            "amount": "{{Sheet1!B4}}",
            "note": "Total {{Sheet1!B4|$}} for {{'Q3 Budget'!C7}}",
            "rows": [["{{Sheet1!B2:B4}}", " {{Sheet1!C1|long}} "]],
        }),
        &[],
    )
    .unwrap();
    assert_eq!(out["amount"], json!(1200.5));
    assert_eq!(out["note"], "Total $1,200.50 for $2,000.00");
    assert_eq!(out["rows"][0][0], json!([400, 300.5, 500]));
    assert_eq!(out["rows"][0][1], "October 23, 2026");
}

#[test]
fn a_general_reference_reads_any_view_at_its_pointer() {
    let out = bound(
        json!({
            "payout": "{{notes:uuid:7/read#/fields/payout}}",
            "net": "{{= {notes:uuid:7/read#/fields/payout} - {notes:uuid:7/read#/fields/excess} |$}}",
            "title": "{{notes:uuid:7/read#/title}}",
        }),
        &[],
    )
    .unwrap();
    assert_eq!(out["payout"], "$4,250.00");
    assert_eq!(out["net"], "$3,750.00");
    assert_eq!(out["title"], "Claim");
}

#[test]
fn expressions_compute_with_references_as_operands() {
    let out = bound(
        json!({
            "sum": "{{= sum(Sheet1!B2:B4) }}",
            "per_head": "{{= round(Sheet1!B4 / 3, 2) }}",
            "share": "{{= Sheet1!B4 / sum(Sheet1!B2:B4) |%}}",
            "noise": "{{= 0.1 + 0.2 }}",
            "tax": "{{= 200 * 7.5% }}",
            "due": "{{= workday(Sheet1!C1, 10) }}",
            "due_long": "{{= add_days(\"2026-10-23\", 7) |long}}",
            "span": "{{= days_between(\"2026-10-01\", Sheet1!C1) }}",
        }),
        &[],
    )
    .unwrap();
    assert_eq!(out["sum"], json!(1200.5));
    assert_eq!(out["per_head"], json!(400.17));
    assert_eq!(out["share"], "100%");
    assert_eq!(out["noise"], json!(0.3));
    assert_eq!(out["tax"], json!(15));
    assert_eq!(out["due"], "2026-11-06");
    assert_eq!(out["due_long"], "October 30, 2026");
    assert_eq!(out["span"], json!(22));
}

#[test]
fn a_parameter_that_takes_text_gets_text_and_one_that_takes_a_number_a_number() {
    let params = [
        ActionParam::required("title", ParamType::Text, "a title"),
        ActionParam::required("count", ParamType::Integer, "a count"),
    ];
    let out = bound(
        json!({"title": "{{= 2 + 3 }}", "count": "{{'Q3 Budget'!C7}}"}),
        &params,
    )
    .unwrap();
    assert_eq!(out["title"], "5");
    assert_eq!(out["count"], json!(2000));
}

#[test]
fn only_the_strict_pattern_binds() {
    let out = bound(
        json!({
            "template": "Hello {{name}}, {not a ref} and {{ also not }}",
            "json": "{\"a\": {\"b\": 1}}",
            "count": 3,
        }),
        &[],
    )
    .unwrap();
    assert_eq!(
        out["template"],
        "Hello {{name}}, {not a ref} and {{ also not }}"
    );
    assert_eq!(out["json"], "{\"a\": {\"b\": 1}}");
    assert!(!binds("Hello {{name}}"));
    assert!(binds("Total {{Sheet1!B4}}"));
}

#[test]
fn a_missing_value_or_a_bad_expression_names_the_reference() {
    let empty = bound(json!({"a": "{{Sheet1!D9}}"}), &[]).unwrap_err();
    assert!(
        empty.contains("{{Sheet1!D9}}") && empty.contains("empty"),
        "{empty}"
    );
    let unknown = bound(json!({"a": "Sum: {{Other!A1}}"}), &[]).unwrap_err();
    assert!(
        unknown.contains("{{Other!A1}}") && unknown.contains("Other!A1"),
        "{unknown}"
    );
    let pointer = bound(json!({"a": "{{notes:uuid:7/read#/fields/nope}}"}), &[]).unwrap_err();
    assert!(pointer.contains("/fields/nope"), "{pointer}");
    let divide = bound(json!({"a": "{{= Sheet1!B4 / 0 }}"}), &[]).unwrap_err();
    assert!(
        divide.contains("{{= Sheet1!B4 / 0}}") && divide.contains("division"),
        "{divide}"
    );
    let list = bound(json!({"a": "{{= Sheet1!B2:B4 * 2 }}"}), &[]).unwrap_err();
    assert!(list.contains("sum, avg"), "{list}");
}

#[test]
fn a_read_quoted_twice_is_read_once() {
    let mut room = room();
    let mut arguments = json!({"a": "{{Sheet1!B4}}", "b": "{{= Sheet1!B4 * 2 }}"})
        .as_object()
        .unwrap()
        .clone();
    bind(&mut arguments, &[], &mut room).unwrap();
    assert_eq!(arguments["b"], json!(2401));
    assert_eq!(room.asked.len(), 1);
}

#[test]
fn formats_write_money_percent_places_and_dates() {
    let cases = [
        (json!(1234.5), "$", "$1,234.50"),
        (json!(-80), "$", "-$80.00"),
        (json!(1234.5), "$0", "$1,235"),
        (json!(0.125), "%", "12.5%"),
        (json!(0.125), "0.0%", "12.5%"),
        (json!(2.5), "0", "3"),
        (json!(1234.5678), "#,##0.00", "1,234.57"),
        (json!(-2.5), "0", "-3"),
        (json!("2026-10-23"), "short", "Oct 23, 2026"),
        (json!("2026-10-23T09:00:00Z"), "iso", "2026-10-23"),
    ];
    for (value, format_as, written) in cases {
        let value = Value::from_json(&value).unwrap();
        assert_eq!(
            format(&value, Some(format_as)).unwrap(),
            written,
            "{format_as}"
        );
    }
}

#[test]
fn business_days_skip_weekends_both_ways() {
    let day = |text: &str| dates::parse(text).unwrap();
    let moved =
        |from: &str, count: i64| dates::iso(dates::add_business_days(day(from), count).unwrap());
    assert_eq!(moved("2026-10-23", 1), "2026-10-26", "Friday to Monday");
    assert_eq!(moved("2026-10-23", 10), "2026-11-06");
    assert_eq!(moved("2026-10-23", -5), "2026-10-16");
    assert_eq!(moved("2026-10-24", 1), "2026-10-26", "from a Saturday");
    assert_eq!(moved("2026-10-24", 0), "2026-10-24");
    assert_eq!(
        dates::business_days_between(day("2026-10-23"), day("2026-10-26")),
        2
    );
    assert_eq!(
        dates::business_days_between(day("2026-10-23"), day("2026-11-06")),
        11
    );
    assert_eq!(
        dates::business_days_between(day("2026-10-26"), day("2026-10-23")),
        -2
    );
    assert!(dates::add_business_days(day("2026-10-23"), 10_000_000).is_none());
}

#[test]
fn dates_read_iso_text_only_and_round_trip() {
    for text in ["1970-01-01", "2000-02-29", "2026-12-31", "1969-07-20"] {
        assert_eq!(dates::iso(dates::parse(text).unwrap()), text);
    }
    for text in ["2026-02-30", "26-10-23", "2026-1-5", "October 23"] {
        assert_eq!(dates::parse(text), None, "{text}");
    }
    assert_eq!(
        dates::parse("2026-10-23T09:00:00Z"),
        dates::parse("2026-10-23")
    );
}

#[test]
fn numbers_read_as_people_write_them() {
    for (text, number) in [
        ("1,200.50", 1200.5),
        ("$4,250", 4250.0),
        ("-€3", -3.0),
        ("12%", 0.12),
        ("(450)", -450.0),
        (" 7 ", 7.0),
    ] {
        assert_eq!(expr::lenient_number(text), Some(number), "{text}");
    }
    for text in ["", "abc", "2026-10-23", "1.2.3"] {
        assert_eq!(expr::lenient_number(text), None, "{text}");
    }
}

#[test]
fn expressions_parse_precedence_and_refuse_what_they_cannot_read() {
    let eval = |source: &str| {
        expr::evaluate(source, &mut |text| Err(format!("no {text}"))).map(|value| value.to_json())
    };
    assert_eq!(eval("1 + 2 * 3").unwrap(), json!(7));
    assert_eq!(eval("(1 + 2) * 3").unwrap(), json!(9));
    assert_eq!(eval("-2 ^ 2").unwrap(), json!(-4));
    assert_eq!(eval("2 ^ 3 ^ 2").unwrap(), json!(512));
    assert_eq!(eval("10 / 4").unwrap(), json!(2.5));
    assert_eq!(eval("max(3, 9, 4) - min(3, 9, 4)").unwrap(), json!(6));
    assert_eq!(eval("avg(2, 4)").unwrap(), json!(3));
    assert_eq!(eval("abs(-3.5)").unwrap(), json!(3.5));
    assert_eq!(eval("round(2.345, 2)").unwrap(), json!(2.35));
    assert_eq!(eval("round(-2.5)").unwrap(), json!(-3));
    assert_eq!(eval("percent(30, 120)").unwrap(), json!(0.25));
    assert_eq!(eval("\"2026-10-23\" + 9").unwrap(), json!("2026-11-01"));
    assert_eq!(
        eval("date(2026, 10, 23) - date(\"2026-10-01\")").unwrap(),
        json!(22)
    );
    assert!(eval("1 +").is_err());
    assert!(eval("(1 + 2").is_err());
    assert!(
        eval("frobnicate(1)")
            .unwrap_err()
            .contains("unknown function")
    );
    assert!(eval("total * 2").unwrap_err().contains("unknown name"));
    assert!(eval("Sheet1!B2 * 2").unwrap_err().contains("no Sheet1!B2"));
    assert!(eval("1 2").unwrap_err().contains("unexpected"));
}

#[test]
fn references_parse_strictly() {
    let cell = parse_reference("'Q3 Budget'!$C$7").unwrap();
    assert_eq!(cell.view.arguments["item"], "Q3 Budget!C7");
    assert_eq!(cell.pointer, "/rows/0/0");
    let view = parse_reference("so.tilo.notes:uuid:9f/notes.page#read?item=p1#/title").unwrap();
    assert_eq!(view.view.app, "so.tilo.notes");
    assert_eq!(view.view.record.as_deref(), Some("uuid:9f"));
    assert_eq!(view.view.operation, "notes.page#read");
    assert_eq!(view.view.arguments["item"], "p1");
    assert_eq!(view.pointer, "/title");
    let picked = parse_reference("sheets/read#").unwrap();
    assert_eq!(picked.view.record, None);
    assert_eq!(picked.pointer, "");
    for text in [
        "name",
        "Sheet 1!B2",
        "Sheet1!B",
        "Sheet1!B0",
        "a/b",
        "x:y/z",
        "Sheet1!ABCD1",
    ] {
        assert_eq!(parse_reference(text), None, "{text}");
    }
}
