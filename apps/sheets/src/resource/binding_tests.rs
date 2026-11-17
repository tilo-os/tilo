//! What a formula's binding holds beyond two members at once: a workbook written before
//! bindings, a line cut from a sheet's end, a reference past it, sheets renamed, deleted and
//! copied, two writers of one cell, and a delete undone. Through the workbook's published
//! actions, on replicas that really merge, read back by the workbook's read.

use automerge::{ROOT, ReadDoc as _, ScalarValue, Value as Stored};
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Doc, Replica, evolution};

use super::concurrent_tests::{Write, app_reads, at, book, replica_reads, replica_writes};
use super::reference_tests::{Change, call, change, confirmed};
use super::{SheetsShell, Workbook};

fn held(formula: &str, shows: &str) -> (String, String) {
    (formula.to_owned(), shows.to_owned())
}

/// What `cell` of `sheet` holds and shows in the app and on the other replica, which must
/// hold the same workbook.
#[track_caller]
fn both(
    app: &mut TestApp<SheetsShell>,
    other: &Replica<Workbook>,
    sheet: &str,
    cell: &str,
) -> (String, String) {
    assert_eq!(
        app.state(),
        other.state().unwrap(),
        "both hold the same workbook"
    );
    let ours = at(&app_reads(app, sheet), cell);
    assert_eq!(
        ours,
        at(&replica_reads(other, sheet), cell),
        "{sheet}!{cell} reads the same on both"
    );
    ours
}

/// The formulas of the workbook that are bound, by cell.
fn bound_cells(book: &Workbook) -> Vec<String> {
    let mut cells = Vec::new();
    for sheet in book.sheets.values() {
        let layout = sheet.layout();
        for (row, column, cell) in sheet.cells.cells() {
            if cell.raw.is_bound()
                && let Some((col, row)) = layout.addr(&row, &column)
            {
                cells.push(format!(
                    "{}!{}",
                    sheet.name.0,
                    crate::model::addr_to_a1(col, row)
                ));
            }
        }
    }
    cells.sort();
    cells
}

/// The document as a build before bindings kept it: every input its text alone, at version 2.
fn as_version_two(doc: &mut Doc) {
    for sheet in evolution::collection_items(doc, &ROOT, "sheets").unwrap() {
        let Some((_, grid)) = doc.get(&sheet, "cells").unwrap() else {
            continue;
        };
        let Some((_, cells)) = doc.get(&grid, "cells").unwrap() else {
            continue;
        };
        for key in doc.keys(&cells).collect::<Vec<_>>() {
            let Some((_, cell)) = doc.get(&cells, key.as_str()).unwrap() else {
                continue;
            };
            let text = match doc.get(&cell, "raw").unwrap() {
                Some((Stored::Scalar(raw), _)) => match raw.as_ref() {
                    ScalarValue::Str(raw) => {
                        raw.split_once('\u{1f}').map(|(text, _)| text.to_owned())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(text) = text {
                evolution::fixture(doc, &cell, &json!({ "raw": text })).unwrap();
            }
        }
    }
    evolution::write_version(doc, 2).unwrap();
    doc.commit();
}

/// A workbook an earlier build wrote opens and computes as it did, with no cell rewritten; a
/// formula gains its binding when it is written, and when rows change under it, so two
/// members who each insert a row at once both keep it on its cells.
#[test]
fn a_workbook_written_before_bindings_reads_as_it_did_and_binds_what_is_written() {
    let mut doc = book().peer_document();
    as_version_two(&mut doc);
    assert_eq!(evolution::stored_version(&doc), 2);
    assert!(
        evolution::read_checked::<Workbook>(&doc).is_err(),
        "it is brought up before it is read"
    );

    let mut app = TestApp::<SheetsShell>::launched_replica(&[], &mut doc).unwrap();
    assert_eq!(app.read_document(evolution::stored_version), 3);
    assert_eq!(
        bound_cells(&app.state()),
        Vec::<String>::new(),
        "opening rewrites no cell"
    );
    let summary = app_reads(&mut app, "Summary");
    assert_eq!(
        (at(&summary, "A1"), at(&summary, "B1")),
        (held("=Data!A3", "2"), held("=SUM(Data!A2:A4)", "6")),
        "it computes as it did"
    );

    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Summary", "start": "C1", "rows": [["=Data!A5+1"]]}),
    );
    assert_eq!(
        bound_cells(&app.state()),
        ["Summary!C1"],
        "a formula written now is bound, the others are as they were"
    );
    let again = call(
        &mut app,
        "set-cells",
        json!({"sheet": "Summary", "start": "A1", "rows": [["=Data!A3"]]}),
    );
    assert_eq!(
        again,
        json!({ "unchanged": true }),
        "a formula written as it reads is no change"
    );

    // Each inserts rows above the data without the other's: on main the old formulas ended
    // rows off, each insert having rewritten them for itself alone.
    let mut other = Replica::<Workbook>::from_doc(app.peer_document());
    call(&mut app, "insert-rows", json!({"sheet": "Data", "at": 1}));
    change(&mut other, &Change::InsertRows { at: 2, count: 2 });
    app.merge_peer(other.doc_mut()).unwrap();
    for (cell, formula, shows) in [
        ("A1", "=Data!A6", "2"),
        ("B1", "=SUM(Data!A5:A7)", "6"),
        ("C1", "=Data!A8+1", "10"),
    ] {
        assert_eq!(
            both(&mut app, &other, "Summary", cell),
            held(formula, shows),
            "{cell}"
        );
    }
    assert_eq!(
        bound_cells(&app.state()),
        ["Summary!A1", "Summary!B1", "Summary!C1"],
        "the formulas the rows moved are bound"
    );
}

/// The last row of a sheet is deleted while a formula pointing at it is written elsewhere: it
/// reads as deleted for everyone, a range ending there shrinks, a reference past the end moves
/// up with the rows, and a row made later is another row.
#[test]
fn a_row_cut_from_the_end_reads_as_deleted_and_no_later_row_takes_its_place() {
    let mut app = book();
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Summary", "start": "C1",
               "rows": [["=Data!A5*2", "=SUM(Data!A2:A5)", "=Data!A6+1"]]}),
    );
    let mut other = Replica::<Workbook>::from_doc(app.peer_document());
    replica_writes(
        &mut other,
        &Write::Cells("Summary", &[("A2", "=Data!A5+1")]),
    );
    confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [5]}),
    );
    app.merge_peer(other.doc_mut()).unwrap();
    for (cell, formula, shows) in [
        ("C1", "=#REF!*2", "#REF!"),
        ("D1", "=SUM(Data!A2:A4)", "6"),
        ("E1", "=Data!A5+1", "1"),
        ("A2", "=#REF!+1", "#REF!"),
    ] {
        assert_eq!(
            both(&mut app, &other, "Summary", cell),
            held(formula, shows),
            "{cell}, with the last row deleted"
        );
    }

    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Data", "start": "A5", "rows": [[7]]}),
    );
    app.merge_peer(other.doc_mut()).unwrap();
    for (cell, formula, shows) in [
        ("C1", "=#REF!*2", "#REF!"),
        ("E1", "=Data!A5+1", "8"),
        ("A2", "=#REF!+1", "#REF!"),
    ] {
        assert_eq!(
            both(&mut app, &other, "Summary", cell),
            held(formula, shows),
            "{cell}, with a new row where the deleted one was"
        );
    }
}

/// A reference past the rows a sheet holds follows a row inserted above it, and reads the row
/// the sheet makes there later.
#[test]
fn a_reference_past_the_sheets_end_follows_the_rows_above_it() {
    let mut app = book();
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Summary", "start": "C1",
               "rows": [["=SUM(Data!A2:A100)", "=Data!A50+1"]]}),
    );
    let mut other = Replica::<Workbook>::from_doc(app.peer_document());
    change(&mut other, &Change::InsertRows { at: 1, count: 1 });
    app.merge_peer(other.doc_mut()).unwrap();
    assert_eq!(
        both(&mut app, &other, "Summary", "C1"),
        held("=SUM(Data!A3:A101)", "15")
    );
    assert_eq!(
        both(&mut app, &other, "Summary", "D1"),
        held("=Data!A51+1", "1")
    );
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Data", "start": "A51", "rows": [[4]]}),
    );
    app.merge_peer(other.doc_mut()).unwrap();
    assert_eq!(
        both(&mut app, &other, "Summary", "C1"),
        held("=SUM(Data!A3:A101)", "19")
    );
    assert_eq!(
        both(&mut app, &other, "Summary", "D1"),
        held("=Data!A51+1", "5")
    );
}

/// A sheet is renamed while a formula naming it by its old name is written elsewhere; then it
/// is deleted while another is written, and a new sheet takes its name.
#[test]
fn a_formula_reads_the_sheet_it_named_whatever_that_is_called_and_not_another() {
    let mut app = book();
    let mut other = Replica::<Workbook>::from_doc(app.peer_document());
    replica_writes(
        &mut other,
        &Write::Cells("Summary", &[("A2", "=Data!A3*10")]),
    );
    call(
        &mut app,
        "rename-sheet",
        json!({"sheet": "Data", "name": "Q3 data"}),
    );
    app.merge_peer(other.doc_mut()).unwrap();
    for (cell, formula, shows) in [
        ("A1", "='Q3 data'!A3", "2"),
        ("B1", "=SUM('Q3 data'!A2:A4)", "6"),
        ("A2", "='Q3 data'!A3*10", "20"),
    ] {
        assert_eq!(
            both(&mut app, &other, "Summary", cell),
            held(formula, shows),
            "{cell}, with the sheet renamed"
        );
    }

    replica_writes(
        &mut other,
        &Write::Cells("Summary", &[("B2", "='Q3 data'!A4+1")]),
    );
    confirmed(&mut app, "delete-sheet", json!({"sheet": "Q3 data"}));
    app.merge_peer(other.doc_mut()).unwrap();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Q3 data", "rows": [["b"], [5], [6], [7]]}),
    );
    app.merge_peer(other.doc_mut()).unwrap();
    for (cell, formula) in [
        ("A1", "=#REF!"),
        ("B1", "=SUM(#REF!)"),
        ("A2", "=#REF!*10"),
        ("B2", "=#REF!+1"),
    ] {
        assert_eq!(
            both(&mut app, &other, "Summary", cell),
            held(formula, "#REF!"),
            "{cell}, with the sheet deleted and another of its name made"
        );
    }
}

/// A copied sheet's formulas read the copy, and follow its rows, not the original's.
#[test]
fn a_copied_sheets_formulas_read_the_copy() {
    let mut app = book();
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Data", "start": "D2", "rows": [["=A3*10", "=Data!A3+1"]]}),
    );
    call(&mut app, "duplicate-sheet", json!({"sheet": "Data"}));
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Data copy", "start": "A3", "rows": [[5]]}),
    );
    call(
        &mut app,
        "insert-rows",
        json!({"sheet": "Data copy", "at": 1}),
    );
    let copy = app_reads(&mut app, "Data copy");
    assert_eq!(
        (at(&copy, "D3"), at(&copy, "E3")),
        (held("=A4*10", "50"), held("=Data!A3+1", "3")),
        "its own references read the copy; one that names the original reads the original"
    );
    let original = app_reads(&mut app, "Data");
    assert_eq!(
        (at(&original, "D2"), at(&original, "E2")),
        (held("=A3*10", "20"), held("=Data!A3+1", "3"))
    );
}

/// Two members write one cell at once while a third inserts a row: the cell ends as one
/// of the two formulas, whole, reading what its writer pointed at; never one's text with the
/// other's cells.
#[test]
fn two_writers_of_one_cell_leave_one_formula_whole() {
    let mut app = book();
    let mut first = Replica::<Workbook>::from_doc(app.peer_document());
    let mut second = Replica::<Workbook>::from_doc(app.peer_document());
    call(&mut app, "insert-rows", json!({"sheet": "Data", "at": 1}));
    replica_writes(
        &mut first,
        &Write::Cells("Summary", &[("A1", "=Data!A3*100")]),
    );
    replica_writes(
        &mut second,
        &Write::Cells("Summary", &[("A1", "=Data!A4+Data!A2")]),
    );
    app.merge_peer(first.doc_mut()).unwrap();
    app.merge_peer(second.doc_mut()).unwrap();
    app.merge_peer(first.doc_mut()).unwrap();
    assert_eq!(first.state().unwrap(), second.state().unwrap());
    let cell = both(&mut app, &first, "Summary", "A1");
    assert!(
        cell == held("=Data!A4*100", "200") || cell == held("=Data!A5+Data!A3", "4"),
        "one writer's formula, on the cells that writer pointed at: {cell:?}"
    );
}

/// A deleted row undone is the row it was: what pointed at it reads it again.
#[test]
fn a_deleted_row_undone_is_read_again() {
    let mut app = book();
    let read = |app: &mut TestApp<SheetsShell>| {
        let summary = app_reads(app, "Summary");
        (at(&summary, "A1"), at(&summary, "B1"))
    };
    let whole = (held("=Data!A3", "2"), held("=SUM(Data!A2:A4)", "6"));
    confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [3, 4]}),
    );
    let cut = (held("=#REF!", "#REF!"), held("=SUM(Data!A2:A2)", "1"));
    assert_eq!(read(&mut app), cut);
    assert!(app.undo(), "the delete is undone");
    assert_eq!(read(&mut app), whole);
    assert!(app.redo(), "and redone");
    assert_eq!(read(&mut app), cut);
}

/// An agent reads the data, a person inserts a row above it, and the agent writes a formula
/// against what it read: the write that names the read's `seen` is refused and says why, with
/// nothing written; read again, it lands on the cells the agent means.
#[test]
fn a_write_against_rows_that_moved_since_its_read_is_refused_and_says_so() {
    let mut app = book();
    let read = call(&mut app, "read", json!({"item": "Data"}));
    let seen = read["seen"].as_str().unwrap().to_owned();

    let answer = call(
        &mut app,
        "set-cells",
        json!({"sheet": "Summary", "start": "A2", "rows": [["=Data!A3*10"]], "seen": seen}),
    );
    assert_eq!(
        (
            answer["formulas"]["A2"].as_str(),
            answer["rows"][0][0].as_f64()
        ),
        (Some("=Data!A3*10"), Some(20.0)),
        "the answer says what the formula reads"
    );
    let seen = answer["seen"].as_str().unwrap().to_owned();
    assert_ne!(
        read["seen"], answer["seen"],
        "the write made a row, and answers where the rows are now"
    );

    let mut person = Replica::<Workbook>::from_doc(app.peer_document());
    change(&mut person, &Change::InsertRows { at: 1, count: 1 });
    app.merge_peer(person.doc_mut()).unwrap();

    let stale = json!({"sheet": "Summary", "start": "B2", "rows": [["=Data!A3*10"]], "seen": seen});
    let refused = app
        .call("set-cells", stale.clone())
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("moved since the read you name with `seen`")
            && refused.contains("nothing was written"),
        "{refused}"
    );
    let cells = json!({"sheet": "Summary", "seen": seen,
                       "cells": [{"cell": "B2", "input": "=Data!A3*10"}]});
    assert!(app.call("write-cells", cells).is_err());
    let summary = app_reads(&mut app, "Summary");
    assert_eq!(at(&summary, "B2"), held("", ""), "nothing was written");
    assert_eq!(
        at(&summary, "A2"),
        held("=Data!A4*10", "20"),
        "the formula written before the row follows it"
    );

    // Read again: the same text now means the cell above, and the write that names this
    // read goes through; one that names no read is not held to any.
    let seen = summary["seen"].as_str().unwrap();
    let mut again = stale;
    again["seen"] = json!(seen);
    let answer = call(&mut app, "set-cells", again);
    assert_eq!(
        (
            answer["formulas"]["B2"].as_str(),
            answer["rows"][0][0].as_f64()
        ),
        (Some("=Data!A3*10"), Some(10.0))
    );
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Summary", "start": "C2", "rows": [["=Data!A4"]]}),
    );
}
