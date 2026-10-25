#![allow(clippy::unwrap_used, reason = "test assertions")]

use serde_json::json;
use tilo_actions::{ActionParam, ParamType};

use super::plain::{Wants, typed};
use super::*;

const DATE: Wants = Wants::Date { time: false };
const NUMBER: Wants = Wants::Number { whole: false };

fn date_of(text: &str) -> serde_json::Value {
    typed(text, DATE).unwrap().unwrap()
}

fn number_of(text: &str) -> serde_json::Value {
    typed(text, NUMBER).unwrap().unwrap()
}

/// What `{{= source }}` computes, with no references.
fn computed(source: &str) -> Value {
    expr::evaluate(source, &mut |text| Err(format!("no `{text}`")))
        .or_else(|why| plain::evaluate(source).map_err(|_| why))
        .unwrap()
}

#[test]
fn a_date_argument_takes_business_days_counted_in_words() {
    // 2026-10-14 is a Wednesday: ten business days on is the 28th.
    for text in [
        "10 business days after 2026-10-14",
        "2026-10-14 + 10 business days",
        "ten working days after October 14, 2026",
        "10 business days from 14 October 2026",
        "10 workdays after the 14th of October 2026",
    ] {
        assert_eq!(date_of(text), json!("2026-10-28"), "{text}");
    }
    // Over a weekend, both ways.
    assert_eq!(
        date_of("1 business day after 2026-10-16"),
        json!("2026-10-19")
    );
    assert_eq!(
        date_of("2 business days before 2026-10-19"),
        json!("2026-10-15")
    );
    assert_eq!(date_of("2026-10-19 - 1 business day"), json!("2026-10-16"));
    // Calendar days, weeks, months (a day the month lacks is its last).
    assert_eq!(date_of("10 days after 2026-10-14"), json!("2026-10-24"));
    assert_eq!(date_of("3 weeks before 2026-10-14"), json!("2026-09-23"));
    assert_eq!(date_of("2026-01-31 + 1 month"), json!("2026-02-28"));
    assert_eq!(date_of("12 months before 2026-10-16"), json!("2025-10-16"));
    // A written date alone is put as ISO; an ISO one is left as it is.
    assert_eq!(date_of("14 October 2026"), json!("2026-10-14"));
    assert_eq!(typed("2026-10-14", DATE).unwrap(), None);
    // A date and time keeps the time written.
    assert_eq!(
        typed(
            "10 business days after 2026-10-14T09:00:00",
            Wants::Date { time: true }
        )
        .unwrap(),
        Some(json!("2026-10-28T09:00:00"))
    );
}

#[test]
fn a_date_argument_never_guesses_the_order_of_day_and_month() {
    let ambiguous = typed("10 business days after 03/04/2026", DATE).unwrap_err();
    assert!(
        ambiguous.contains("2026-04-03") && ambiguous.contains("2026-03-04"),
        "{ambiguous}"
    );
    assert!(ambiguous.contains("ISO"), "{ambiguous}");
    // Only one order is a date: no guess.
    assert_eq!(date_of("14/10/2026"), json!("2026-10-14"));
    assert_eq!(date_of("10/14/2026 + 1 day"), json!("2026-10-15"));
    for unread in ["next Friday", "soon", "2026-10-14 + 10", "10 business days"] {
        let why = typed(unread, DATE).unwrap_err();
        assert!(
            why.contains("10 business days after 2026-10-14"),
            "{unread}: {why}"
        );
    }
}

#[test]
fn a_number_argument_takes_arithmetic_in_words_and_marks() {
    assert_eq!(number_of("289 - 50"), json!(239));
    assert_eq!(number_of("4 x 38.50"), json!(154));
    assert_eq!(number_of("4 × 38.50"), json!(154));
    assert_eq!(number_of("€289 - €50"), json!(239));
    assert_eq!(number_of("EUR 289 minus EUR 50"), json!(239));
    assert_eq!(number_of("(120 + 80) / 2"), json!(100));
    assert_eq!(number_of("1,200.50 + 99.50"), json!(1300));
    assert_eq!(number_of("sum of 120, 80 and 45"), json!(245));
    assert_eq!(number_of("total of 120 and 80"), json!(200));
    assert_eq!(number_of("average of 120, 80 and 100"), json!(100));
    assert_eq!(
        number_of("max of 3,412.50, 3,980 and 3,412.50"),
        json!(3980)
    );
    assert_eq!(
        number_of("(3412.50 + 3412.50 + 3980) / 3 * 12"),
        json!(43_220)
    );
    assert_eq!(number_of("15% of 200"), json!(30));
    assert_eq!(number_of("100 divided by 8"), json!(12.5));
    // A plain number is the app's to read.
    assert_eq!(typed("239", NUMBER).unwrap(), None);
    assert_eq!(typed("$1,200", NUMBER).unwrap(), None);
}

#[test]
fn a_number_argument_refuses_what_it_cannot_read_whole() {
    let comma = typed("38,50 + 1", NUMBER).unwrap_err();
    assert!(comma.contains("decimals with a point"), "{comma}");
    for unread in [
        "lots",
        "289 - ",
        "289 - fifty",
        "sum of",
        "2026-10-14 + 2 days",
    ] {
        let why = typed(unread, NUMBER).unwrap_err();
        assert!(why.contains("289 - 50"), "{unread}: {why}");
    }
    let whole = typed("7 / 2", Wants::Number { whole: true }).unwrap_err();
    assert!(whole.contains("3.5"), "{whole}");
    assert!(typed("1 / 0", NUMBER).is_err());
}

#[test]
fn params_say_which_arguments_take_dates_and_numbers() {
    let pattern = |pattern: &str| {
        ActionParam::required_owned("on", ParamType::Text, "")
            .with_schema(json!({"type": "string", "pattern": pattern}))
    };
    let date = pattern(r"^\d{4}-\d{2}-\d{2}$");
    let date_time = pattern(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?$");
    let formatted = ActionParam::required_owned("on", ParamType::Text, "")
        .with_schema(json!({"type": ["string", "null"], "format": "date"}));
    assert_eq!(plain::param_wants(&date), Some(DATE));
    assert_eq!(
        plain::param_wants(&date_time),
        Some(Wants::Date { time: true })
    );
    assert_eq!(plain::param_wants(&formatted), Some(DATE));
    assert_eq!(
        plain::param_wants(&ActionParam::required("at", ParamType::DateTime, "")),
        Some(Wants::Date { time: true })
    );
    assert_eq!(
        plain::param_wants(&ActionParam::required("n", ParamType::Integer, "")),
        Some(Wants::Number { whole: true })
    );
    assert_eq!(
        plain::param_wants(&ActionParam::required("text", ParamType::Text, "")),
        None
    );
}

#[test]
fn only_typed_arguments_compute_and_text_stays_text() {
    let params = [
        ActionParam::required_owned("due", ParamType::Text, "")
            .with_schema(json!({"type": "string", "pattern": r"^\d{4}-\d{2}-\d{2}$"})),
        ActionParam::required("payout", ParamType::Number, ""),
        ActionParam::required("note", ParamType::Text, ""),
        ActionParam::required_owned("claim", ParamType::Object, "").with_schema(json!({
            "type": "object",
            "properties": {
                "excess": {"type": "integer"},
                "items": {"type": "array", "items": {"type": "number"}},
                "title": {"type": "string"},
            },
        })),
    ];
    let mut arguments = json!({
        "due": "10 business days after 2026-10-14",
        "payout": "289 - 50",
        "note": "10 business days after 2026-10-14, 289 - 50",
        "claim": {"excess": "25 x 2", "items": ["120 + 80", 45], "title": "289 - 50"},
    })
    .as_object()
    .cloned()
    .unwrap();
    fill_plain(&mut arguments, &params).unwrap();
    assert_eq!(
        serde_json::Value::Object(arguments),
        json!({
            "due": "2026-10-28",
            "payout": 239,
            "note": "10 business days after 2026-10-14, 289 - 50",
            "claim": {"excess": 50, "items": [200, 45], "title": "289 - 50"},
        })
    );
    let mut wrong = json!({"due": "03/04/2026"}).as_object().cloned().unwrap();
    let why = fill_plain(&mut wrong, &params).unwrap_err();
    assert!(why.starts_with("`due`:"), "{why}");
}

#[test]
fn an_expression_may_say_its_arithmetic_in_words() {
    assert_eq!(
        computed("10 business days after 2026-10-14"),
        Value::Date(dates::parse("2026-10-28").unwrap())
    );
    assert_eq!(computed("sum of 120, 80 and 45"), Value::Number(245.0));
    // The expression's own error stands when the words are not read
    // either.
    let why = expr::evaluate("nonsense(1)", &mut |_| Err(String::new())).unwrap_err();
    assert!(why.contains("unknown function"), "{why}");
}

#[test]
fn days_total_across_ranges_with_their_ends_counted_as_said() {
    let trips =
        r#""2025-07-20", "2025-08-12", "2026-01-05", "2026-01-15", "2026-05-09", "2026-05-14""#;
    assert_eq!(
        computed(&format!("days_inclusive({trips})")),
        Value::Number(41.0)
    );
    assert_eq!(
        computed(&format!("days_exclusive({trips})")),
        Value::Number(38.0)
    );
    // Only the days inside a window: the 12 months before 2026-10-16.
    assert_eq!(
        computed(&format!(
            r#"days_within(add_months("2026-10-16", -12), "2026-10-16", {trips})"#
        )),
        Value::Number(17.0)
    );
    // A range the window cuts counts its days inside.
    assert_eq!(
        computed(r#"days_within("2025-08-01", "2026-10-16", "2025-07-20", "2025-08-12")"#),
        Value::Number(12.0)
    );
    assert_eq!(
        computed(r#"add_years("2028-02-29", 1)"#),
        Value::Date(dates::parse("2029-02-28").unwrap())
    );
    let odd = expr::evaluate(r#"days_inclusive("2026-01-05")"#, &mut |_| {
        Err(String::new())
    })
    .unwrap_err();
    assert!(odd.contains("pairs"), "{odd}");
    let backwards = expr::evaluate(r#"days_inclusive("2026-01-15", "2026-01-05")"#, &mut |_| {
        Err(String::new())
    })
    .unwrap_err();
    assert!(backwards.contains("ends before it starts"), "{backwards}");
}

#[test]
fn ranges_come_from_a_range_of_cells_too() {
    let mut room = Room::default();
    room.cells.insert(
        "Trips!B2:C4".into(),
        json!([
            ["2025-07-20", "2025-08-12"],
            ["2026-01-05", "2026-01-15"],
            ["2026-05-09", "2026-05-14"]
        ]),
    );
    let params = [ActionParam::required("days", ParamType::Integer, "")];
    let mut arguments = json!({
        "days": "{{= days_within(add_months(\"2026-10-16\", -12), \"2026-10-16\", Trips!B2:C4) }}"
    })
    .as_object()
    .cloned()
    .unwrap();
    bind(&mut arguments, &params, &mut room).unwrap();
    assert_eq!(arguments["days"], json!(17));
}

/// A room answering only cell reads.
#[derive(Default)]
struct Room {
    cells: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Reads for Room {
    fn read(&mut self, view: &ViewRef) -> Result<serde_json::Value, String> {
        let item = view.arguments.get("item").cloned().unwrap_or_default();
        self.cells
            .get(&item)
            .map(|rows| json!({"rows": rows}))
            .ok_or_else(|| format!("no `{item}`"))
    }
}

#[test]
fn a_document_s_dates_are_noted_in_the_order_it_writes_them() {
    let details = "Date of birth: 03/04/1988\nLiving there since: 14/08/2024";
    assert_eq!(
        annotate_text(details),
        "Date of birth: 03/04/1988 [1988-04-03]\nLiving there since: 14/08/2024 [2024-08-14]"
    );
    // Nothing settles the order: both readings, named.
    assert_eq!(
        annotate_text("issued 03/04/2026"),
        "issued 03/04/2026 [2026-04-03 if day first, 2026-03-04 if month first]"
    );
    // Dots are written day first; a stated order settles it too.
    assert_eq!(
        annotate_text("Rechnung vom 03.04.2026"),
        "Rechnung vom 03.04.2026 [2026-04-03]"
    );
    assert_eq!(
        annotate_text("Dates are MM/DD/YYYY. Born 03/04/1988."),
        "Dates are MM/DD/YYYY. Born 03/04/1988 [1988-03-04]."
    );
    // Months by name; ISO is left as it is.
    assert_eq!(
        annotate_text("Paid on 14 October 2026, due Oct. 28, 2026, sent 2026-10-14."),
        "Paid on 14 October 2026 [2026-10-14], due Oct. 28, 2026 [2026-10-28], sent 2026-10-14."
    );
    assert_eq!(
        annotate_text("the 3rd of May 2026"),
        "the 3rd of May 2026 [2026-05-03]"
    );
    // Noting twice changes nothing.
    let once = annotate_text(details);
    assert_eq!(annotate_text(&once), once);
}

#[test]
fn amounts_are_noted_where_their_separators_could_be_misread() {
    assert_eq!(
        annotate_text("Total €1.234,50, excess EUR 50,00 and £3,412.50 gross"),
        "Total €1.234,50 [1234.50], excess EUR 50,00 [50.00] and £3,412.50 [3412.50] gross"
    );
    assert_eq!(annotate_text("289,00 EUR"), "289,00 EUR [289.00]");
    // Plain already, or either way: no note.
    assert_eq!(
        annotate_text("EUR 289.00 and €1.234"),
        "EUR 289.00 and €1.234"
    );
}

#[test]
fn one_document_settles_the_order_for_all_its_texts() {
    let read = json!({"paragraphs": ["Born 03/04/1988", "Moved in 14/08/2024"], "pages": 2});
    assert_eq!(
        annotate_json(&read),
        json!({"paragraphs": ["Born 03/04/1988 [1988-04-03]", "Moved in 14/08/2024 [2024-08-14]"],
               "pages": 2})
    );
}

#[test]
fn notes_copied_into_a_step_come_out_again() {
    assert_eq!(
        unannotated("Born 03/04/1988 [1988-04-03] in Recife, paid €1.234,50 [1234.50]."),
        Some("Born 03/04/1988 in Recife, paid €1.234,50.".to_owned())
    );
    assert_eq!(
        unannotated(&annotate_text("issued 03/04/2026")),
        Some("issued 03/04/2026".to_owned())
    );
    // Brackets that are not the host's notes stay.
    assert_eq!(unannotated("See [1] and 14 October 2026 [draft]"), None);
    assert_eq!(unannotated("no notes"), None);
}
