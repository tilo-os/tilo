use std::io::Read;

use serde_json::json;
use ui::tilo_resource::{Id, Invoke, Replica};

use super::*;
use crate::resource::verbs;
use crate::workbook::Workbook;

/// A workbook whose first sheet, "Data", holds `rows` from A1.
fn workbook(rows: &[&[&str]]) -> Replica<Workbook> {
    let mut book = Replica::<Workbook>::new();
    let cells = (0u32..)
        .zip(rows)
        .flat_map(|(r, row)| {
            (0u32..)
                .zip(row.iter())
                .map(move |(c, raw)| CellWrite::at((c, r), *raw, None))
        })
        .collect();
    book.apply(Workbook::IMPORT_SHEETS.with(verbs::ImportSheets {
        sheets: vec![crate::resource::ImportedSheet {
            name: "Data".into(),
            cells,
            hidden: false,
        }],
        source: None,
        title: None,
    }))
    .unwrap();
    book
}

fn sheet(book: &Replica<Workbook>) -> Id {
    book.state().unwrap().sheets.ids().next().unwrap().clone()
}

fn snapshot(book: &Replica<Workbook>) -> SheetSnapshot {
    book.state()
        .unwrap()
        .sheets
        .values()
        .next()
        .unwrap()
        .snapshot()
}

fn read_back(book: &Replica<Workbook>, range: Option<&str>) -> serde_json::Value {
    read(&snapshot(book), range).unwrap()
}

fn write(book: &mut Replica<Workbook>, cells: Vec<CellWrite>) {
    let sheet = sheet(book);
    book.apply(Workbook::WRITE_CELLS.with(verbs::WriteCells { sheet, cells }))
        .unwrap();
}

fn format(
    book: &mut Replica<Workbook>,
    range: &str,
    format: Option<CellFormat>,
    bold: Option<bool>,
) {
    let sheet = sheet(book);
    book.apply(Workbook::FORMAT_CELLS.with(verbs::FormatCells {
        sheet,
        range: range.into(),
        format,
        bold,
    }))
    .unwrap();
}

fn raws(book: &Replica<Workbook>) -> Vec<Vec<String>> {
    let snap = snapshot(book);
    let rows = snap.raw.keys().map(|addr| addr.1).max().unwrap_or(0);
    let cols = snap.raw.keys().map(|addr| addr.0).max().unwrap_or(0);
    (0..=rows)
        .map(|r| {
            (0..=cols)
                .map(|c| snap.raw.get(&(c, r)).cloned().unwrap_or_default())
                .collect()
        })
        .collect()
}

fn cell(reference: &str, input: &str) -> CellWrite {
    CellWrite {
        cell: reference.into(),
        input: Some(input.into()),
        ..CellWrite::default()
    }
}

#[test]
fn a_read_answers_the_used_cells() {
    let book = workbook(&[&["Region", "Q1"], &["North", "120"]]);
    assert_eq!(read_back(&book, None)["range"], "A1:B2");
}

#[test]
fn written_values_and_formulas_read_back_computed() {
    let mut book = workbook(&[&["Region", "Q1"], &["North", "120"], &["South", "90"]]);
    write(
        &mut book,
        vec![cell("A4", "Total"), cell("B4", "=SUM(B2:B3)")],
    );
    write(
        &mut book,
        vec![cell("C1", "Q2"), cell("C2", "135"), cell("C4", "TRUE")],
    );
    let page = read_back(&book, None);
    assert_eq!(page["range"], "A1:C4");
    assert_eq!(
        page["rows"],
        json!([
            ["Region", "Q1", "Q2"],
            ["North", 120.0, 135.0],
            ["South", 90.0, null],
            ["Total", 210.0, true]
        ])
    );
    assert_eq!(page["formulas"], json!({"B4": "=SUM(B2:B3)"}));
    assert_eq!(read_back(&book, Some("B4"))["rows"], json!([[210.0]]));
}

#[test]
fn bold_is_stored_read_back_and_exported_to_xlsx() {
    let mut book = workbook(&[&["Region", "Q1"], &["North", "120"]]);
    format(&mut book, "A1:B1", None, Some(true));
    format(
        &mut book,
        "B2",
        Some(CellFormat::Currency(tilo_locale::Currency::USD)),
        None,
    );
    assert_eq!(read_back(&book, None)["bold"], json!(["A1", "B1"]));

    let snap = snapshot(&book);
    let values = crate::formula::recalculate(&snap.raw);
    let table =
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &snap.fmt, &values, &snap.bold);
    let path = std::env::temp_dir().join(format!("sheets-bold-{}.xlsx", uuid::Uuid::new_v4()));
    crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut part = |name: &str| {
        let mut out = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        out
    };
    let styles = part("xl/styles.xml");
    let sheet = part("xl/worksheets/sheet1.xml");
    assert!(styles.contains(r"<font><b/>"), "{styles}");
    // A1 and B1 share the bold style; B2 has the currency style only.
    assert!(
        sheet.contains(r#"<c r="A1" t="inlineStr" s="1">"#),
        "{sheet}"
    );
    assert!(
        sheet.contains(r#"<c r="B1" t="inlineStr" s="1">"#),
        "{sheet}"
    );
    assert!(sheet.contains(r#"<c r="B2" s="2">"#), "{sheet}");
    assert!(
        styles.contains(
            r#"<xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>"#
        ),
        "{styles}"
    );

    format(&mut book, "A1", None, Some(false));
    assert_eq!(read_back(&book, None)["bold"], json!(["B1"]));
}

#[test]
fn sort_rows_compares_formula_values_not_formatted_or_rounded_labels() {
    for format_kind in [
        CellFormat::Number,
        CellFormat::Currency(tilo_locale::Currency::USD),
        CellFormat::Percent,
    ] {
        let mut book = workbook(&[
            &["ten", "=10"],
            &["two", "=2"],
            &["thousand", "=1000"],
            &["three", "3"],
            &["larger fraction", "=1.004"],
            &["smaller fraction", "=1.003"],
        ]);
        format(&mut book, "B1:B6", Some(format_kind), None);
        let before = snapshot(&book);
        for descending in [false, true] {
            let sheet = sheet(&book);
            book.apply(Workbook::SORT_ROWS.with(verbs::SortRows {
                sheet,
                range: "A1:B6".into(),
                by: "B".into(),
                descending: Some(descending),
            }))
            .unwrap();
            let mut expected = vec![
                "smaller fraction",
                "larger fraction",
                "two",
                "three",
                "ten",
                "thousand",
            ];
            if descending {
                expected.reverse();
            }
            let actual: Vec<_> = raws(&book).into_iter().map(|row| row[0].clone()).collect();
            assert_eq!(actual, expected, "{format_kind:?}, descending={descending}");
            let after = snapshot(&book);
            assert_eq!(after.fmt, before.fmt, "sorting does not change the format");
            assert_eq!(
                read_back(&book, Some("B1"))["rows"][0][0],
                if descending {
                    json!(1000.0)
                } else {
                    json!(1.003)
                }
            );
        }
    }
}

#[test]
fn sort_rows_reorders_by_a_column_and_moves_formulas_with_their_row() {
    let mut book = workbook(&[
        &["name", "signed_up", "n"],
        &["Ada", "2026-03-02", "=ROW()"],
        &["Grace", "2026-01-15", "1"],
        &["Alan", "2026-02-20", "=B4"],
    ]);
    let sort = |book: &mut Replica<Workbook>, by: &str, descending: Option<bool>| {
        let sheet = sheet(book);
        book.apply(Workbook::SORT_ROWS.with(verbs::SortRows {
            sheet,
            range: "A2:C4".into(),
            by: by.into(),
            descending,
        }))
        .unwrap();
    };
    sort(&mut book, "B", None);
    assert_eq!(
        raws(&book),
        [
            ["name", "signed_up", "n"],
            ["Grace", "2026-01-15", "1"],
            ["Alan", "2026-02-20", "=B3"],
            ["Ada", "2026-03-02", "=ROW()"],
        ]
    );
    sort(&mut book, "A", Some(true));
    assert_eq!(
        raws(&book)
            .iter()
            .skip(1)
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        ["Grace", "Alan", "Ada"]
    );
}

#[test]
fn delete_rows_removes_several_rows_in_one_step_and_moves_bold() {
    let mut book = workbook(&[&["h"], &["a"], &["dup"], &["b"], &["dup"], &["c"]]);
    format(&mut book, "A6", None, Some(true));
    let sheet = sheet(&book);
    book.apply_confirmed(Workbook::DELETE_ROWS.with(verbs::DeleteRows {
        sheet,
        rows: vec![5, 3],
    }))
    .unwrap();
    assert_eq!(raws(&book), [["h"], ["a"], ["b"], ["c"]]);
    assert_eq!(read_back(&book, None)["bold"], json!(["A4"]));
}

/// What `invoke` (cell edit `action`) answers its caller with, as the door
/// computes it from the change it applied.
fn answer(
    book: &mut Replica<Workbook>,
    action: &str,
    invoke: Invoke<Workbook>,
) -> serde_json::Value {
    let before = book.heads();
    book.apply_confirmed(invoke).unwrap();
    let after = book.heads();
    let changes = book.doc_mut().diff(&before, &after);
    super::answer(book.doc(), action, &changes).unwrap_or(serde_json::Value::Null)
}

fn set_cells(book: &Replica<Workbook>, start: &str, rows: serde_json::Value) -> Invoke<Workbook> {
    let rows = serde_json::from_value(rows).unwrap();
    Workbook::SET_CELLS.with(verbs::SetCells {
        sheet: sheet(book),
        start: start.into(),
        rows,
    })
}

#[test]
fn cell_edits_answer_with_the_range_they_changed() {
    let mut book = workbook(&[&["Region", "Q1"], &["North", "120"], &["South", "90"]]);
    let invoke = set_cells(&book, "A4", json!([["Total", "=SUM(B2:B3)"]]));
    let set = answer(&mut book, "set-cells", invoke);
    assert_eq!(
        set,
        json!({"sheet": "Data", "item": sheet(&book).as_str(), "range": "A4:B4",
            "rows": [["Total", 210.0]], "formulas": {"B4": "=SUM(B2:B3)"}, "bold": []})
    );
    let invoke = Workbook::FORMAT_CELLS.with(verbs::FormatCells {
        sheet: sheet(&book),
        range: "A1:B1".into(),
        format: None,
        bold: Some(true),
    });
    let bolded = answer(&mut book, "format-cells", invoke);
    assert_eq!(bolded["range"], "A1:B1");
    assert_eq!(bolded["bold"], json!(["A1", "B1"]));
    let invoke = Workbook::SORT_ROWS.with(verbs::SortRows {
        sheet: sheet(&book),
        range: "A2:B3".into(),
        by: "B".into(),
        descending: None,
    });
    let sorted = answer(&mut book, "sort-rows", invoke);
    assert_eq!(sorted["range"], "A2:B3");
    assert_eq!(sorted["rows"], json!([["South", 90.0], ["North", 120.0]]));
    let invoke = Workbook::DELETE_ROWS.with(verbs::DeleteRows {
        sheet: sheet(&book),
        rows: vec![1],
    });
    let deleted = answer(&mut book, "delete-rows", invoke);
    // Deleting rows moves everything below, so it answers what remains from
    // the top.
    assert_eq!(deleted["range"], "A1:B3");
    assert_eq!(
        deleted["rows"],
        json!([["South", 90.0], ["North", 120.0], ["Total", 210.0]])
    );
    assert_eq!(deleted["formulas"], json!({"B3": "=SUM(B1:B2)"}));
    assert_eq!(deleted["bold"], json!([]));
    assert!(sorted.get("changed").is_none());
}

#[test]
fn a_large_edit_answers_its_first_rows_and_names_the_whole_range() {
    let mut book = workbook(&[&["x"]]);
    let rows: Vec<serde_json::Value> = (0..100).map(|row| json!([row, row, row, row])).collect();
    let invoke = set_cells(&book, "A1", json!(rows));
    let set = answer(&mut book, "set-cells", invoke);
    assert_eq!(set["changed"], "A1:D100");
    assert_eq!(set["range"], "A1:D64");
    assert_eq!(set["rows"].as_array().map(Vec::len), Some(64));

    let wide: Vec<serde_json::Value> = (0..300).map(|col| json!(col)).collect();
    let invoke = set_cells(&book, "A200", json!([wide]));
    let set = answer(&mut book, "set-cells", invoke);
    assert_eq!(set["changed"], "A200:KN200");
    assert_eq!(set["rows"][0].as_array().map(Vec::len), Some(256));
}

#[test]
fn the_app_answers_a_cell_edit_through_its_action_door() {
    use ui::framework::testing::TestApp;
    let mut app = TestApp::<crate::SheetsShell>::new().unwrap();
    let sheet = app.state().sheets.ids().next().unwrap().clone();
    let item = ui::tilo_resource::items::item_resource_id::<Workbook>("sheets", &sheet);
    let value = app
        .call_on(
            &item,
            "set-cells",
            json!({"start": "Z90", "rows": [["=1+2"]]}),
        )
        .unwrap();
    assert_eq!(
        value
            .as_ref()
            .map(|value| (&value["range"], &value["rows"])),
        Some((&json!("Z90"), &json!([[3.0]]))),
        "{value:?}"
    );
}

#[test]
fn only_the_cell_edits_declare_an_answer() {
    use ui::tilo_actions::{OperationKind, ResultSpec};
    use ui::tilo_resource::Resource as _;
    let book = workbook(&[&["x"]]);
    let state = book.state().unwrap();
    let published = ui::tilo_resource::publish::<Workbook>(Workbook::KIND, Workbook::LABEL, &state);
    let item = ui::tilo_resource::items::item_resource_id::<Workbook>("sheets", &sheet(&book));
    let sheet = published
        .children
        .iter()
        .find(|child| child.id.as_str() == item)
        .expect("the sheet is published");
    for spec in &sheet.actions {
        let answers = matches!(spec.contract.result, ResultSpec::Json { .. })
            && spec.kind == OperationKind::Document;
        assert_eq!(
            answers,
            ANSWERING_EDITS.contains(&spec.id.as_str()),
            "{}",
            spec.id
        );
    }
}

#[test]
fn malformed_arguments_are_refused_by_name() {
    let mut book = workbook(&[&["x"]]);
    let sheet = sheet(&book);
    for (invoke, message) in [
        (
            Workbook::WRITE_CELLS.with(verbs::WriteCells {
                sheet: sheet.clone(),
                cells: vec![cell("nowhere", "x")],
            }),
            "not a cell",
        ),
        (
            Workbook::FORMAT_CELLS.with(verbs::FormatCells {
                sheet: sheet.clone(),
                range: "A1".into(),
                format: None,
                bold: None,
            }),
            "give a format, bold or both",
        ),
        (
            Workbook::SORT_ROWS.with(verbs::SortRows {
                sheet: sheet.clone(),
                range: "A1:B3".into(),
                by: "D".into(),
                descending: None,
            }),
            "inside the range",
        ),
        (
            Workbook::DELETE_ROWS.with(verbs::DeleteRows {
                sheet,
                rows: vec![0],
            }),
            "numbered from 1",
        ),
    ] {
        let invoke: Invoke<Workbook> = invoke;
        let error = book.apply_confirmed(invoke).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn a_read_stays_bounded() {
    let book = workbook(&[&["x"]]);
    assert!(read(&snapshot(&book), Some("A1:Z1000")).is_err());
    assert!(read(&snapshot(&book), Some("nowhere")).is_err());
}

#[test]
fn an_import_into_a_blank_workbook_replaces_its_empty_sheet() {
    let import = |book: &mut Replica<Workbook>| {
        book.apply(Workbook::IMPORT_SHEETS.with(verbs::ImportSheets {
            sheets: vec![crate::resource::ImportedSheet {
                name: "sales".into(),
                cells: vec![cell("A1", "Region")],
                hidden: false,
            }],
            source: None,
            title: None,
        }))
        .unwrap();
    };
    let names = |book: &Replica<Workbook>| -> Vec<String> {
        book.state()
            .unwrap()
            .sheets
            .values()
            .map(|sheet| sheet.name.0.clone())
            .collect()
    };
    let mut blank = Replica::<Workbook>::new();
    import(&mut blank);
    assert_eq!(names(&blank), ["sales"]);

    let mut used = workbook(&[&["kept"]]);
    import(&mut used);
    assert_eq!(names(&used), ["Data", "sales"]);
}

/// A formula that does not parse is kept and shows its error, as in any
/// spreadsheet, and the edit answers which cell and why, so the writer can
/// fix it; the cells beside it are written as usual.
#[test]
fn a_formula_that_does_not_parse_is_kept_and_answered() {
    let mut book = workbook(&[&["Item", "Price"], &["x", "4"]]);
    let invoke = set_cells(
        &book,
        "A4",
        json!([["Total", "=INDEX(A1:A5, MATCH(\"x\", B1:B5, 0)", "=B2*2"]]),
    );
    let set = answer(&mut book, "set-cells", invoke);
    assert_eq!(
        set["rows"],
        json!([["Total", "#ERROR!", 8.0]]),
        "the broken formula is stored and shows its error"
    );
    assert_eq!(
        set["formula_errors"],
        json!([{"cell": "B4", "shows": "#ERROR!",
            "message": "`INDEX(` is never closed: add `)`"}])
    );
    assert_eq!(
        read_back(&book, Some("A4:C4"))["formula_errors"],
        set["formula_errors"],
        "reading the cells reports it too"
    );
    assert!(
        read_back(&book, Some("A1:B2"))
            .get("formula_errors")
            .is_none(),
        "a range without broken formulas has no formula_errors"
    );
}
