//! Two members in one workbook at once: one changes a sheet's rows or columns while the
//! other, who has not received that change, writes a formula. Once both hold both changes, the
//! formula reads the cells its writer pointed at, on both replicas. Every case goes through the
//! workbook's published actions, or the grid's own keys, on two replicas that really merge, and
//! is read back by the workbook's read.

use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Id, Replica, Resource as _};

use super::reference_tests::{Change, call, change, confirmed};
use super::{CellWrite, SheetsShell, Workbook, verbs};

/// `Data` holds a header and four rows under it: a, 1, 2, 3, 9 down column A and k, x, y, x, z beside
/// them. `Summary` reads it with two formulas both members hold.
pub(super) fn book() -> TestApp<SheetsShell> {
    let mut app = TestApp::<SheetsShell>::new().unwrap();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Data", "rows": [["a", "k"], [1, "x"], [2, "y"], [3, "x"], [9, "z"]]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Summary", "rows": [["=Data!A3", "=SUM(Data!A2:A4)"]]}),
    );
    app
}

/// The cells every read here covers, from A1 so a cell is found by its own address.
const WINDOW: &str = "A1:H12";

pub(super) fn app_reads(app: &mut TestApp<SheetsShell>, sheet: &str) -> Value {
    call(app, "read", json!({ "item": format!("{sheet}!{WINDOW}") }))
}

/// The same read on a replica: a member with no app at all.
pub(super) fn replica_reads(book: &Replica<Workbook>, sheet: &str) -> Value {
    let state = book.state().unwrap();
    Workbook::read_content(&state, book.doc(), Some(&format!("{sheet}!{WINDOW}"))).unwrap()
}

/// The formula `cell` holds and what it shows, in a read of [`WINDOW`].
pub(super) fn at(read: &Value, cell: &str) -> (String, String) {
    let (col, row) = crate::model::a1_to_addr(cell).unwrap();
    let shows = read["rows"][row as usize][col as usize]
        .as_str()
        .unwrap_or_default();
    let formula = read["formulas"][cell].as_str().unwrap_or_default();
    (formula.to_owned(), shows.to_owned())
}

/// What a member writes: cells one by one, or rows from a start cell.
pub(super) enum Write {
    Cells(&'static str, &'static [(&'static str, &'static str)]),
    Rows(&'static str, &'static str, &'static [&'static str]),
}

/// The write through the typed action on a replica, as an agent's session applies it.
pub(super) fn replica_writes(book: &mut Replica<Workbook>, write: &Write) {
    let done = match write {
        Write::Cells(sheet, cells) => book.apply(
            Workbook::WRITE_CELLS.with(verbs::WriteCells {
                sheet: Id::from_raw(*sheet),
                cells: cells
                    .iter()
                    .map(|(cell, input)| CellWrite {
                        cell: (*cell).to_owned(),
                        input: Some((*input).to_owned()),
                        ..CellWrite::default()
                    })
                    .collect(),
                seen: None,
            }),
        ),
        Write::Rows(sheet, start, row) => book.apply(Workbook::SET_CELLS.with(verbs::SetCells {
            sheet: Id::from_raw(*sheet),
            start: (*start).to_owned(),
            rows: vec![row.iter().map(|input| json!(input)).collect()],
            seen: None,
        })),
    };
    if let Err(error) = done {
        panic!("the write: {error:?}");
    }
}

/// The same write called on the running app, as an agent calls it there.
pub(super) fn app_writes(app: &mut TestApp<SheetsShell>, write: &Write) {
    match write {
        Write::Cells(sheet, cells) => {
            let cells: Vec<Value> = cells
                .iter()
                .map(|(cell, input)| json!({ "cell": cell, "input": input }))
                .collect();
            call(
                app,
                "write-cells",
                json!({ "sheet": sheet, "cells": cells }),
            );
        }
        Write::Rows(sheet, start, row) => {
            call(
                app,
                "set-cells",
                json!({ "sheet": sheet, "start": start, "rows": [row] }),
            );
        }
    }
}

/// The change to `Data` called on the running app.
fn app_changes(app: &mut TestApp<SheetsShell>, change: &Change) {
    match change {
        Change::InsertRows { at, count } => call(
            app,
            "insert-rows",
            json!({ "sheet": "Data", "at": at, "count": count }),
        ),
        Change::DeleteRows(rows) => {
            confirmed(app, "delete-rows", json!({ "sheet": "Data", "rows": rows }))
        }
        Change::InsertColumns(at) => {
            call(app, "insert-columns", json!({ "sheet": "Data", "at": at }))
        }
        Change::DeleteColumns(columns) => confirmed(
            app,
            "delete-columns",
            json!({ "sheet": "Data", "columns": columns }),
        ),
    };
}

struct Case {
    name: &'static str,
    /// What one member does to `Data`.
    change: Change,
    /// What the other writes before that change reaches them.
    writes: &'static [Write],
    /// Once both hold both: sheet, cell, the formula as it reads there and what it shows.
    meant: &'static [(&'static str, &'static str, &'static str, &'static str)],
}

const CASES: &[Case] = &[
    Case {
        name: "a row inserted above the data",
        change: Change::InsertRows { at: 1, count: 1 },
        writes: &[
            Write::Cells(
                "Summary",
                &[
                    ("A2", "=Data!A3*10"),
                    ("B2", "=SUM(Data!A2:A4)*10"),
                    ("C2", "=Data!$A$3+1"),
                    ("D2", "=SUM(Data!A:A)"),
                    ("E2", "=SUM(Data!2:2)"),
                ],
            ),
            Write::Rows("Data", "D2", &["=A3*10", "=SUM($A$2:$A$4)"]),
        ],
        meant: &[
            ("Summary", "A2", "=Data!A4*10", "20"),
            ("Summary", "B2", "=SUM(Data!A3:A5)*10", "60"),
            ("Summary", "C2", "=Data!$A$4+1", "3"),
            ("Summary", "D2", "=SUM(Data!A:A)", "15"),
            // The whole row, with the two formulas written into it at the same time.
            ("Summary", "E2", "=SUM(Data!3:3)", "27"),
            ("Data", "D3", "=A4*10", "20"),
            ("Data", "E3", "=SUM($A$3:$A$5)", "6"),
            ("Summary", "A1", "=Data!A4", "2"),
            ("Summary", "B1", "=SUM(Data!A3:A5)", "6"),
        ],
    },
    Case {
        name: "two rows inserted inside the range",
        change: Change::InsertRows { at: 3, count: 2 },
        writes: &[
            Write::Rows("Summary", "A2", &["=SUM(Data!A2:A4)", "=Data!A3"]),
            Write::Cells("Data", &[("D5", "=A5-A2")]),
        ],
        meant: &[
            ("Summary", "A2", "=SUM(Data!A2:A6)", "6"),
            ("Summary", "B2", "=Data!A5", "2"),
            ("Data", "D7", "=A7-A2", "8"),
        ],
    },
    Case {
        name: "a column inserted left of the data",
        change: Change::InsertColumns("A"),
        writes: &[
            Write::Cells(
                "Summary",
                &[
                    ("A2", "=Data!A3*10"),
                    ("B2", "=SUM(Data!A:A)"),
                    ("C2", "=SUMIF(Data!B2:B5,\"x\",Data!A2:A5)"),
                ],
            ),
            Write::Cells("Data", &[("D2", "=A3*10")]),
        ],
        meant: &[
            ("Summary", "A2", "=Data!B3*10", "20"),
            ("Summary", "B2", "=SUM(Data!B:B)", "15"),
            ("Summary", "C2", "=SUMIF(Data!C2:C5,\"x\",Data!B2:B5)", "4"),
            ("Data", "E2", "=B3*10", "20"),
            ("Summary", "A1", "=Data!B3", "2"),
        ],
    },
    Case {
        name: "a row above deleted",
        change: Change::DeleteRows(&[2]),
        writes: &[
            Write::Cells(
                "Summary",
                &[
                    ("A2", "=Data!A4*10"),
                    ("B2", "=SUM(Data!A3:A5)"),
                    ("C2", "=Data!$A$5"),
                ],
            ),
            Write::Cells("Data", &[("D4", "=A4*10")]),
        ],
        meant: &[
            ("Summary", "A2", "=Data!A3*10", "30"),
            ("Summary", "B2", "=SUM(Data!A2:A4)", "14"),
            ("Summary", "C2", "=Data!$A$4", "9"),
            ("Data", "D3", "=A3*10", "30"),
            ("Summary", "A1", "=Data!A2", "2"),
            ("Summary", "B1", "=SUM(Data!A2:A3)", "5"),
        ],
    },
    Case {
        name: "the row pointed at deleted",
        change: Change::DeleteRows(&[3]),
        writes: &[Write::Cells(
            "Summary",
            &[
                ("A2", "=Data!A3*10"),
                ("B2", "=Data!A4"),
                ("C2", "=SUM(Data!A2:A4)"),
            ],
        )],
        meant: &[
            ("Summary", "A2", "=#REF!*10", "#REF!"),
            ("Summary", "B2", "=Data!A3", "3"),
            ("Summary", "C2", "=SUM(Data!A2:A3)", "4"),
            ("Summary", "A1", "=#REF!", "#REF!"),
            ("Summary", "B1", "=SUM(Data!A2:A3)", "4"),
        ],
    },
    Case {
        name: "the first row of a range deleted",
        change: Change::DeleteRows(&[2]),
        writes: &[Write::Cells("Summary", &[("A2", "=SUM(Data!A2:A4)")])],
        meant: &[("Summary", "A2", "=SUM(#REF!)", "#REF!")],
    },
    Case {
        name: "a column deleted",
        change: Change::DeleteColumns(&["A"]),
        writes: &[Write::Cells(
            "Summary",
            &[
                ("A2", "=Data!B3"),
                ("B2", "=Data!A3*10"),
                ("C2", "=COUNTA(Data!B:B)"),
            ],
        )],
        meant: &[
            ("Summary", "A2", "=Data!A3", "y"),
            ("Summary", "B2", "=#REF!*10", "#REF!"),
            ("Summary", "C2", "=COUNTA(Data!A:A)", "5"),
            ("Summary", "A1", "=#REF!", "#REF!"),
        ],
    },
    Case {
        name: "a formula both hold written again",
        change: Change::InsertRows { at: 1, count: 1 },
        writes: &[Write::Cells(
            "Summary",
            &[("A1", "=Data!A3*100"), ("B1", "=SUM(Data!A2:A3)")],
        )],
        meant: &[
            ("Summary", "A1", "=Data!A4*100", "200"),
            ("Summary", "B1", "=SUM(Data!A3:A4)", "3"),
        ],
    },
];

/// One member changes `Data` while the other writes formulas through the typed actions,
/// each way round: the change in the running app and the writes on another replica, then the
/// change on the replica and the writes in the app.
#[test]
fn a_formula_written_while_rows_or_columns_change_reads_what_its_writer_pointed_at() {
    let mut wrong = Vec::new();
    for case in CASES {
        for changed_in_app in [true, false] {
            let mut app = book();
            let mut other = Replica::<Workbook>::from_doc(app.peer_document());
            let who = if changed_in_app {
                app_changes(&mut app, &case.change);
                for write in case.writes {
                    replica_writes(&mut other, write);
                }
                "changed in the app, written on the other replica"
            } else {
                change(&mut other, &case.change);
                for write in case.writes {
                    app_writes(&mut app, write);
                }
                "changed on the other replica, written in the app"
            };
            app.merge_peer(other.doc_mut()).unwrap();
            if app.state() != other.state().unwrap() {
                wrong.push(format!("{} ({who}): the replicas differ", case.name));
                continue;
            }
            for (sheet, cell, formula, shows) in case.meant {
                let meant = ((*formula).to_owned(), (*shows).to_owned());
                let ours = at(&app_reads(&mut app, sheet), cell);
                let theirs = at(&replica_reads(&other, sheet), cell);
                if ours != meant || theirs != meant {
                    wrong.push(format!(
                        "{} ({who}): {sheet}!{cell} is meant to hold {formula} and show {shows}; \
                         the app holds {} and shows {}, the other replica holds {} and shows {}",
                        case.name, ours.0, ours.1, theirs.0, theirs.1
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} cells read other cells than their writer pointed at:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// A person types a formula into the grid while an agent, on another replica, inserts a row
/// above the cells it points at.
#[test]
fn a_formula_a_person_types_while_a_row_is_inserted_reads_what_they_pointed_at() {
    let mut app = book();
    app.harness().resize((1200, 800)).unwrap();
    app.settle();
    let mut agent = Replica::<Workbook>::from_doc(app.peer_document());
    change(&mut agent, &Change::InsertRows { at: 1, count: 1 });

    let summary = app
        .state()
        .sheets
        .iter()
        .find(|(_, sheet)| sheet.name.0 == "Summary")
        .map(|(id, _)| id.clone())
        .unwrap();
    for (cell, typed) in [((0, 1), "=Data!A3*10"), ((1, 1), "=SUM(Data!A2:A4)*10")] {
        let anchor = super::cell_anchor(&summary, cell);
        call(&mut app, "open", json!({ "anchor": anchor }));
        app.settle();
        app.harness().type_text(typed);
        app.harness().key("Enter");
        app.settle();
    }
    let typed = app_reads(&mut app, "Summary");
    assert_eq!(
        (at(&typed, "A2"), at(&typed, "B2")),
        (
            ("=Data!A3*10".to_owned(), "20".to_owned()),
            ("=SUM(Data!A2:A4)*10".to_owned(), "60".to_owned())
        ),
        "what the person typed and saw before the row reached them"
    );

    app.merge_peer(agent.doc_mut()).unwrap();
    assert_eq!(app.state(), agent.state().unwrap());
    for (cell, formula, shows) in [
        ("A2", "=Data!A4*10", "20"),
        ("B2", "=SUM(Data!A3:A5)*10", "60"),
    ] {
        let meant = (formula.to_owned(), shows.to_owned());
        assert_eq!(
            at(&app_reads(&mut app, "Summary"), cell),
            meant,
            "{cell} in the app"
        );
        assert_eq!(
            at(&replica_reads(&agent, "Summary"), cell),
            meant,
            "{cell} on the agent's replica"
        );
    }
}

/// Undo and redo of the row keep a formula written meanwhile on the cells its writer pointed
/// at, for both members.
#[test]
fn undo_and_redo_of_the_row_keep_a_formula_written_meanwhile_on_its_cells() {
    let mut app = book();
    let mut other = Replica::<Workbook>::from_doc(app.peer_document());
    call(&mut app, "insert-rows", json!({ "sheet": "Data", "at": 1 }));
    replica_writes(
        &mut other,
        &Write::Cells(
            "Summary",
            &[("A2", "=Data!A3*10"), ("B2", "=SUM(Data!A2:A4)*10")],
        ),
    );
    let both_read = |app: &mut TestApp<SheetsShell>,
                     other: &mut Replica<Workbook>,
                     meant: [(&str, &str); 2],
                     when: &str| {
        app.merge_peer(other.doc_mut()).unwrap();
        assert_eq!(app.state(), other.state().unwrap(), "{when}");
        let meant: Vec<(String, String)> = meant
            .iter()
            .map(|(formula, shows)| ((*formula).to_owned(), (*shows).to_owned()))
            .collect();
        let read = |read: &Value| vec![at(read, "A2"), at(read, "B2")];
        assert_eq!(
            read(&app_reads(app, "Summary")),
            meant,
            "{when}, in the app"
        );
        assert_eq!(
            read(&replica_reads(other, "Summary")),
            meant,
            "{when}, on the other replica"
        );
    };
    let moved = [("=Data!A4*10", "20"), ("=SUM(Data!A3:A5)*10", "60")];
    let written = [("=Data!A3*10", "20"), ("=SUM(Data!A2:A4)*10", "60")];
    both_read(&mut app, &mut other, moved, "with the row");
    assert!(app.undo(), "the row is undone");
    both_read(&mut app, &mut other, written, "with the row undone");
    assert!(app.redo(), "the row is redone");
    both_read(&mut app, &mut other, moved, "with the row redone");
}
