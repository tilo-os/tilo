//! Formulas that read another sheet, through changes to that sheet: every case goes through the
//! workbook's published actions, as an agent or a person's command calls them, and is read back
//! by the record's read, which needs no window.

use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Id, Replica, Resource as _};

use super::{CellWrite, SheetsShell, Workbook, verbs};

fn call(app: &mut TestApp<SheetsShell>, id: &str, arguments: Value) -> Value {
    app.call(id, arguments)
        .unwrap_or_else(|error| panic!("{id}: {error}"))
        .unwrap_or(Value::Null)
}

/// A call its caller confirmed, as a delete asks.
fn confirmed(app: &mut TestApp<SheetsShell>, id: &str, arguments: Value) -> Value {
    app.call_confirmed(id, arguments)
        .unwrap_or_else(|error| panic!("{id}: {error}"))
        .unwrap_or(Value::Null)
}

/// What the cells of `sheet` show and the formulas behind them, as the `read` action answers.
fn shown(app: &mut TestApp<SheetsShell>, sheet: &str) -> (Value, Value) {
    let read = call(app, "read", json!({ "item": sheet }));
    (read["rows"].clone(), read["formulas"].clone())
}

/// The same read on a replica: a participant with no app at all.
fn read(book: &Replica<Workbook>, sheet: &str) -> (Value, Value) {
    let state = book.state().unwrap();
    let content = Workbook::read_content(&state, book.doc(), Some(sheet)).unwrap();
    (content["rows"].clone(), content["formulas"].clone())
}

/// The six ways `Summary` reads `Data`, in A1 to F1: a cell, a range, an anchored cell, a
/// function over two ranges, a whole column under the name in another case, and a quoted name
/// beside a reference of its own sheet.
const SUMMARY: [&str; 6] = [
    "=Data!A3",
    "=SUM(Data!A2:A4)",
    "=Data!$A$3",
    "=SUMIF(Data!B2:B4,\"x\",Data!A2:A4)",
    "=SUM(data!A:A)",
    "=A1+'Data'!A2",
];

/// What they show while `Data` is as written.
const SHOWN: [&str; 6] = ["2", "6", "2", "4", "15", "3"];

/// `Data` holds a header and four records, and two formulas of its own in D1 and E1: one names
/// its own sheet, one reads `Summary`.
fn book() -> TestApp<SheetsShell> {
    let mut app = TestApp::<SheetsShell>::new().unwrap();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Data", "rows": [
            ["a", "k", null, "=Data!A3+A3", "=Summary!A1"],
            [1, "x"],
            [2, "y"],
            [3, "x"],
            [9, "z"],
        ]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Summary", "rows": [SUMMARY]}),
    );
    app
}

/// What a read of `Summary` answers when its first row holds `formulas` and shows `shows`.
fn expected(formulas: [&str; 6], shows: [&str; 6]) -> (Value, Value) {
    let held: serde_json::Map<String, Value> = ["A1", "B1", "C1", "D1", "E1", "F1"]
        .into_iter()
        .zip(formulas)
        .map(|(cell, formula)| (cell.to_owned(), formula.into()))
        .collect();
    (json!([shows]), held.into())
}

#[derive(Debug)]
enum Change {
    InsertRows { at: u32, count: u32 },
    DeleteRows(&'static [u32]),
    InsertColumns(&'static str),
    DeleteColumns(&'static [&'static str]),
}

fn change(book: &mut Replica<Workbook>, change: &Change) {
    let sheet = Id::from_raw("Data");
    let done = match change {
        Change::InsertRows { at, count } => {
            book.apply(Workbook::INSERT_ROWS.with(verbs::InsertRows {
                sheet,
                at: *at,
                count: Some(*count),
            }))
        }
        Change::DeleteRows(rows) => {
            book.apply_confirmed(Workbook::DELETE_ROWS.with(verbs::DeleteRows {
                sheet,
                rows: rows.to_vec(),
            }))
        }
        Change::InsertColumns(at) => {
            book.apply(Workbook::INSERT_COLUMNS.with(verbs::InsertColumns {
                sheet,
                at: (*at).to_owned(),
                count: None,
            }))
        }
        Change::DeleteColumns(columns) => {
            book.apply_confirmed(Workbook::DELETE_COLUMNS.with(verbs::DeleteColumns {
                sheet,
                columns: columns.iter().map(|column| (*column).to_owned()).collect(),
            }))
        }
    };
    if let Err(error) = done {
        panic!("{change:?}: {error:?}");
    }
}

/// The audit's case, call for call: after a row is inserted above the data, the summary reads
/// the cells it read before; after the row it points at is deleted, it says so.
#[test]
fn a_row_inserted_on_one_sheet_keeps_another_sheets_formulas_on_their_cells() {
    let mut app = TestApp::<SheetsShell>::new().unwrap();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Data", "rows": [["a"], [1], [2], [3]]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Summary", "rows": [["=Data!A3", "=SUM(Data!A2:A4)"]]}),
    );
    assert_eq!(shown(&mut app, "Summary").0, json!([["2", "6"]]));

    call(&mut app, "insert-rows", json!({"sheet": "Data", "at": 1}));
    assert_eq!(
        shown(&mut app, "Summary"),
        (
            json!([["2", "6"]]),
            json!({"A1": "=Data!A4", "B1": "=SUM(Data!A3:A5)"})
        ),
        "the formulas followed their cells down"
    );

    confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [4]}),
    );
    assert_eq!(
        shown(&mut app, "Summary"),
        (
            json!([["#REF!", "4"]]),
            json!({"A1": "=#REF!", "B1": "=SUM(Data!A3:A4)"})
        ),
        "a deleted cell is an error, never the cell that slid into its place"
    );
}

/// What a cell without its target shows.
const GONE: &str = "#REF!";

/// A change to `Data`, and what `Summary` holds and shows after it.
type Case = (&'static str, Change, [&'static str; 6], [&'static str; 6]);

/// Rows and columns inserted and deleted above, inside and below what the summary reads.
const CASES: [Case; 15] = [
    (
        "a row inserted above",
        Change::InsertRows { at: 1, count: 1 },
        [
            "=Data!A4",
            "=SUM(Data!A3:A5)",
            "=Data!$A$4",
            "=SUMIF(Data!B3:B5,\"x\",Data!A3:A5)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A3",
        ],
        SHOWN,
    ),
    (
        "a row inserted inside the range",
        Change::InsertRows { at: 3, count: 1 },
        [
            "=Data!A4",
            "=SUM(Data!A2:A5)",
            "=Data!$A$4",
            "=SUMIF(Data!B2:B5,\"x\",Data!A2:A5)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A2",
        ],
        SHOWN,
    ),
    (
        "a row inserted below",
        Change::InsertRows { at: 5, count: 1 },
        SUMMARY,
        SHOWN,
    ),
    (
        "two rows inserted above",
        Change::InsertRows { at: 2, count: 2 },
        [
            "=Data!A5",
            "=SUM(Data!A4:A6)",
            "=Data!$A$5",
            "=SUMIF(Data!B4:B6,\"x\",Data!A4:A6)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A4",
        ],
        SHOWN,
    ),
    (
        "a row deleted above",
        Change::DeleteRows(&[1]),
        [
            "=Data!A2",
            "=SUM(Data!A1:A3)",
            "=Data!$A$2",
            "=SUMIF(Data!B1:B3,\"x\",Data!A1:A3)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A1",
        ],
        SHOWN,
    ),
    (
        "the row pointed at deleted",
        Change::DeleteRows(&[3]),
        [
            "=#REF!",
            "=SUM(Data!A2:A3)",
            "=#REF!",
            "=SUMIF(Data!B2:B3,\"x\",Data!A2:A3)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A2",
        ],
        [GONE, "4", GONE, "4", "13", GONE],
    ),
    (
        "the last row of the range deleted",
        Change::DeleteRows(&[4]),
        [
            "=Data!A3",
            "=SUM(Data!A2:A3)",
            "=Data!$A$3",
            "=SUMIF(Data!B2:B3,\"x\",Data!A2:A3)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A2",
        ],
        ["2", "3", "2", "1", "12", "3"],
    ),
    (
        "a row deleted below",
        Change::DeleteRows(&[5]),
        SUMMARY,
        ["2", "6", "2", "4", "6", "3"],
    ),
    (
        "every row of the range deleted",
        Change::DeleteRows(&[2, 3, 4]),
        [
            "=#REF!",
            "=SUM(#REF!)",
            "=#REF!",
            "=SUMIF(#REF!,\"x\",#REF!)",
            "=SUM(data!A:A)",
            "=A1+#REF!",
        ],
        [GONE, GONE, GONE, GONE, "9", GONE],
    ),
    (
        "a column inserted left",
        Change::InsertColumns("A"),
        [
            "=Data!B3",
            "=SUM(Data!B2:B4)",
            "=Data!$B$3",
            "=SUMIF(Data!C2:C4,\"x\",Data!B2:B4)",
            "=SUM(data!B:B)",
            "=A1+'Data'!B2",
        ],
        SHOWN,
    ),
    (
        "a column inserted between the two read",
        Change::InsertColumns("B"),
        [
            "=Data!A3",
            "=SUM(Data!A2:A4)",
            "=Data!$A$3",
            "=SUMIF(Data!C2:C4,\"x\",Data!A2:A4)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A2",
        ],
        SHOWN,
    ),
    (
        "a column inserted right",
        Change::InsertColumns("F"),
        SUMMARY,
        SHOWN,
    ),
    (
        "the column pointed at deleted",
        Change::DeleteColumns(&["A"]),
        [
            "=#REF!",
            "=SUM(#REF!)",
            "=#REF!",
            "=SUMIF(Data!A2:A4,\"x\",#REF!)",
            "=SUM(#REF!)",
            "=A1+#REF!",
        ],
        [GONE; 6],
    ),
    (
        "the column of the criteria deleted",
        Change::DeleteColumns(&["B"]),
        [
            "=Data!A3",
            "=SUM(Data!A2:A4)",
            "=Data!$A$3",
            "=SUMIF(#REF!,\"x\",Data!A2:A4)",
            "=SUM(data!A:A)",
            "=A1+'Data'!A2",
        ],
        ["2", "6", "2", GONE, "15", "3"],
    ),
    (
        "a column deleted right",
        Change::DeleteColumns(&["C"]),
        SUMMARY,
        SHOWN,
    ),
];

/// Every case of [`CASES`] starts from the same workbook and is judged at the end, so one run
/// names all that fail.
#[test]
fn formulas_follow_rows_and_columns_of_the_sheet_they_read() {
    let app = book();
    let start = Replica::<Workbook>::from_doc(app.peer_document());
    assert_eq!(
        read(&start, "Summary"),
        expected(SUMMARY, SHOWN),
        "the workbook as written"
    );
    let total = CASES.len();
    let mut wrong = Vec::new();
    for (what, edit, formulas, shows) in &CASES {
        let mut book = Replica::<Workbook>::from_doc(app.peer_document());
        change(&mut book, edit);
        let (seen, held) = read(&book, "Summary");
        let (values, texts) = expected(*formulas, *shows);
        if (&seen, &held) != (&values, &texts) {
            wrong.push(format!(
                "{what}:\n  shows    {seen}\n  expected {values}\n  holds    {held}\n  expected {texts}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {total} changes to Data left Summary wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// On the changed sheet itself a reference that names the sheet moves as a bare one does, and one
/// that reads another sheet stays.
#[test]
fn a_sheets_own_formulas_move_by_name_and_keep_what_reads_elsewhere() {
    let app = book();
    let mut book = Replica::<Workbook>::from_doc(app.peer_document());
    change(&mut book, &Change::InsertRows { at: 1, count: 1 });
    assert_eq!(
        read(&book, "Data").1,
        json!({"D2": "=Data!A4+A4", "E2": "=Summary!A1"})
    );

    let mut book = Replica::<Workbook>::from_doc(app.peer_document());
    change(&mut book, &Change::DeleteRows(&[3]));
    assert_eq!(
        read(&book, "Data").1,
        json!({"D1": "=#REF!+#REF!", "E1": "=Summary!A1"})
    );
}

/// Undo takes the other sheets' formulas back with the row, and redo moves them again: the
/// rewrite is part of the one recorded change.
#[test]
fn undo_and_redo_take_the_other_sheets_formulas_with_the_row() {
    let mut app = book();
    let written = shown(&mut app, "Summary");
    assert_eq!(written, expected(SUMMARY, SHOWN));

    call(&mut app, "insert-rows", json!({"sheet": "Data", "at": 1}));
    let moved = shown(&mut app, "Summary");
    assert_eq!(moved.1["A1"], "=Data!A4");
    assert_eq!(moved.0, written.0, "the same values, one row down");
    assert!(app.undo(), "the insert is one step");
    assert_eq!(shown(&mut app, "Summary"), written, "undone");
    assert!(app.redo());
    assert_eq!(shown(&mut app, "Summary"), moved, "redone");

    confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [4]}),
    );
    let lost = shown(&mut app, "Summary");
    assert_eq!(lost.1["A1"], "=#REF!");
    assert_eq!(lost.0[0][0], "#REF!");
    assert!(app.undo(), "the delete is one step");
    assert_eq!(
        shown(&mut app, "Summary"),
        moved,
        "the row is back and so is the reference to it"
    );
    assert!(app.redo());
    assert_eq!(shown(&mut app, "Summary"), lost);
}

/// A delete tells its caller which formulas it left without a target, on every sheet, and
/// answers about the sheet the rows left.
#[test]
fn a_delete_names_the_formulas_it_left_without_a_target() {
    let mut app = book();
    let answer = confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [3]}),
    );
    assert_eq!(answer["sheet"], "Data", "{answer}");
    assert_eq!(
        answer["reference_errors"]["cells"],
        json!(["Data!D1", "Summary!A1", "Summary!C1"]),
        "{answer}"
    );

    let mut app = book();
    let answer = confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [5]}),
    );
    assert_eq!(answer["sheet"], "Data", "{answer}");
    assert!(answer.get("reference_errors").is_none(), "{answer}");
}

/// A removed sheet leaves the reference error in every formula that read it, and a new sheet of
/// its name does not quietly take its place.
#[test]
fn a_removed_sheet_leaves_the_reference_error() {
    let mut app = book();
    confirmed(&mut app, "delete-sheet", json!({"sheet": "Data"}));
    let gone = expected(
        [
            "=#REF!",
            "=SUM(#REF!)",
            "=#REF!",
            "=SUMIF(#REF!,\"x\",#REF!)",
            "=SUM(#REF!)",
            "=A1+#REF!",
        ],
        ["#REF!"; 6],
    );
    assert_eq!(shown(&mut app, "Summary"), gone);
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Data", "rows": [[7], [7], [7], [7]]}),
    );
    assert_eq!(
        shown(&mut app, "Summary"),
        gone,
        "another sheet of the name is not the sheet they read"
    );
    assert!(app.undo(), "the new sheet");
    assert!(app.undo(), "the delete");
    assert_eq!(
        shown(&mut app, "Summary"),
        expected(SUMMARY, SHOWN),
        "the sheet is back and so are the references to it"
    );
}

/// A sort moves what cells hold and no reference follows it, as in every spreadsheet; its
/// answer names the formulas outside the sorted rows that now read other rows' values.
#[test]
fn a_sort_keeps_references_on_their_addresses_and_says_which() {
    let mut app = book();
    let answer = call(
        &mut app,
        "sort-rows",
        json!({"sheet": "Data", "range": "A2:B5", "by": "A", "descending": true}),
    );
    assert_eq!(
        shown(&mut app, "Summary"),
        expected(SUMMARY, ["3", "14", "3", "3", "15", "12"]),
        "the formulas are as written and read what the sort put at their addresses"
    );
    assert_eq!(
        answer["references"]["cells"],
        json!([
            "Data!D1",
            "Summary!A1",
            "Summary!B1",
            "Summary!C1",
            "Summary!D1",
            "Summary!F1"
        ]),
        "all but the whole column, which reads every sorted row as before: {answer}"
    );
    assert!(
        answer["references"]["how"]
            .as_str()
            .is_some_and(|how| how.contains("sort")),
        "{answer}"
    );

    let unread = call(
        &mut app,
        "sort-rows",
        json!({"sheet": "Summary", "range": "A1:F1", "by": "A"}),
    );
    assert!(unread.get("references").is_none(), "{unread}");
}

/// Two participants at once: one inserts a row above the data while the other, who does not
/// have that row yet, writes formulas that read the data from the summary. Both end with the
/// same workbook; every formula that was there follows the row for both. A formula written
/// meanwhile is held as its writer wrote it: its text names cells by position, and no change its
/// writer had not seen rewrites it, so it reads the cells now at that position, one row above
/// those its writer pointed at. The same holds for a formula on the changed sheet itself.
/// Following the row there too needs references kept by the row's and column's ids beside the
/// text, which is not built.
#[test]
fn a_row_inserted_while_another_writes_a_formula_across_sheets() {
    let mut app = book();
    let mut peer = Replica::<Workbook>::from_doc(app.peer_document());

    call(&mut app, "insert-rows", json!({"sheet": "Data", "at": 1}));
    peer.apply(Workbook::WRITE_CELLS.with(verbs::WriteCells {
        sheet: Id::from_raw("Summary"),
        cells: vec![
            CellWrite::at((0, 1), "=Data!A3*10", None),
            CellWrite::at((1, 1), "=SUM(Data!A2:A4)*10", None),
        ],
    }))
    .unwrap();
    let written = read(&peer, "Summary");
    assert_eq!(written.0[1], json!(["20", "60", "", "", "", ""]));

    app.merge_peer(peer.doc_mut()).unwrap();
    assert_eq!(
        app.state(),
        peer.state().unwrap(),
        "both hold the same workbook"
    );
    let (rows, formulas) = shown(&mut app, "Summary");
    assert_eq!(
        (rows.clone(), formulas.clone()),
        read(&peer, "Summary"),
        "and read the same from it"
    );
    assert_eq!(
        (&rows[0], &formulas["A1"], &formulas["B1"]),
        (
            &json!(SHOWN),
            &json!("=Data!A4"),
            &json!("=SUM(Data!A3:A5)")
        ),
        "the formulas both had follow the row for both"
    );
    assert_eq!(
        (&formulas["A2"], &formulas["B2"]),
        (&written.1["A2"], &written.1["B2"]),
        "the formulas written meanwhile are held as written"
    );

    // The other way round: the row is deleted while a formula that reads it is written.
    let mut app = book();
    let mut peer = Replica::<Workbook>::from_doc(app.peer_document());
    confirmed(
        &mut app,
        "delete-rows",
        json!({"sheet": "Data", "rows": [3]}),
    );
    peer.apply(Workbook::WRITE_CELLS.with(verbs::WriteCells {
        sheet: Id::from_raw("Summary"),
        cells: vec![CellWrite::at((0, 1), "=Data!A2*10", None)],
    }))
    .unwrap();
    app.merge_peer(peer.doc_mut()).unwrap();
    assert_eq!(app.state(), peer.state().unwrap());
    let (rows, formulas) = shown(&mut app, "Summary");
    assert_eq!(
        (&rows[0][0], &formulas["A1"], &formulas["B1"]),
        (
            &json!("#REF!"),
            &json!("=#REF!"),
            &json!("=SUM(Data!A2:A3)")
        ),
        "what read the deleted row says so for both"
    );
    assert_eq!(
        (&rows[1][0], &formulas["A2"]),
        (&json!("10"), &json!("=Data!A2*10")),
        "a formula written meanwhile above the deleted row reads its cell"
    );
}
