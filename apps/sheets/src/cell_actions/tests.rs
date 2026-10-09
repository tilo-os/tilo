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
        }],
        source: None,
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
        Some(CellFormat::Currency(crate::model::Currency::USD)),
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
            }],
            source: None,
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
