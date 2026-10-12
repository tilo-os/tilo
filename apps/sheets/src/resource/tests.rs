//! The workbook through the door the host uses: its verbs and field edits,
//! the invariants, merging, search and history.

use super::*;
use crate::workbook::{FIRST_SHEET_ID, Sheet};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Edit, Replica, Resource};

fn sample() -> Replica<Workbook> {
    crate::sample::replica()
}

fn budget() -> Id {
    Id::from_raw(crate::sample::BUDGET_SHEET_ID)
}

fn sheet_of<'a>(book: &'a Workbook, id: &Id) -> &'a Sheet {
    book.sheet(id).expect("the sheet")
}

fn raw(book: &Replica<Workbook>, sheet: &Id, a1: &str) -> String {
    let state = book.state().unwrap();
    sheet_of(&state, sheet)
        .snapshot()
        .raw
        .get(&a1_to_addr(a1).unwrap())
        .cloned()
        .unwrap_or_default()
}

fn write(book: &mut Replica<Workbook>, sheet: &Id, cells: &[(&str, &str)]) {
    let cells = cells
        .iter()
        .map(|(cell, input)| CellWrite {
            cell: (*cell).to_owned(),
            input: Some((*input).to_owned()),
            ..CellWrite::default()
        })
        .collect();
    book.apply(Workbook::WRITE_CELLS.with(verbs::WriteCells {
        sheet: sheet.clone(),
        cells,
    }))
    .unwrap();
}

fn edit(book: &mut Replica<Workbook>, edit: &Edit<Workbook>) {
    book.apply(Invoke::edit(edit).unwrap()).unwrap();
}

/// A new workbook is one empty first sheet; nothing is fabricated.
#[test]
fn a_new_workbook_opens_one_blank_sheet() {
    let app = TestApp::<SheetsShell>::new().unwrap();
    let state = app.state();
    let names: Vec<&str> = state
        .sheets
        .values()
        .map(|sheet| sheet.name.0.as_str())
        .collect();
    assert_eq!(names, ["Sheet1"]);
    assert!(state.is_blank());
    assert_eq!(state.sheets.ids().next().unwrap().as_str(), FIRST_SHEET_ID);
}

/// Screenshots show the sample workbook, staged through its actions.
#[test]
fn a_capture_stages_the_sample_workbook() {
    let plain = TestApp::<SheetsShell>::capture(None).unwrap();
    let state = plain.state();
    let names: Vec<&str> = state
        .sheets
        .values()
        .map(|sheet| sheet.name.0.as_str())
        .collect();
    assert_eq!(names, ["Budget", "Sales"]);
    assert!(sheet_of(&state, &budget()).charts.is_empty());

    let charts = TestApp::<SheetsShell>::capture(Some("charts")).unwrap();
    let state = charts.state();
    let sheet = sheet_of(&state, &budget());
    assert_eq!(sheet.charts.len(), 2);
    let comments = Sheet::comment_snapshots(&budget(), &state.comments, &sheet.layout());
    assert_eq!(comments.len(), 1);
    assert!(comments[0].resolved);
    assert_eq!(comments[0].author, "Alex", "signed by who wrote it");
    assert_eq!(comments[0].replies[0].author, "Sam");
    assert_eq!(
        (comments[0].start, comments[0].end),
        ((1, 1), (4, 1)),
        "anchored at B2:E2"
    );
}

/// A comment is signed by whoever invokes it, never by a name its
/// arguments carry; an agent comments in one call.
#[test]
fn a_comment_is_signed_by_its_invoker() {
    let mut app = TestApp::<SheetsShell>::new().unwrap();
    let sheet = app.state().sheets.ids().next().unwrap().clone();
    app.invoke(Workbook::COMMENT.with(verbs::Comment {
        sheet: sheet.clone(),
        range: "A1".into(),
        text: "Looks right".into(),
    }))
    .unwrap();
    let state = app.state();
    let comments: Vec<_> = state.comments.values().collect();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].author.0, "test-author");

    let item = tilo_resource::items::item_resource_id::<Workbook>("sheets", &sheet);
    app.call_on(
        &item,
        "write-cells",
        serde_json::json!({"cells": [{"cell": "B2", "input": "=1+2"}]}),
    )
    .unwrap();
    let state = app.state();
    assert_eq!(
        sheet_of(&state, &sheet)
            .snapshot()
            .raw
            .get(&(1, 1))
            .map(String::as_str),
        Some("=1+2"),
        "an agent writes a cell in one call"
    );
}

/// The fill-series scenario extends its seeds through the real fill
/// handle, and the paste scenario adjusts the copied formula: both are
/// edits the capture's document holds.
#[test]
fn document_scenarios_edit_through_the_dispatcher() {
    let raw = |app: &TestApp<SheetsShell>, a1: &str| {
        let state = app.state();
        sheet_of(&state, &budget())
            .snapshot()
            .raw
            .get(&a1_to_addr(a1).unwrap())
            .cloned()
    };
    let fill = TestApp::<SheetsShell>::capture(Some("fillseries")).unwrap();
    assert_eq!(raw(&fill, "B21").as_deref(), Some("6"));
    assert_eq!(raw(&fill, "C21").as_deref(), Some("Sat"));
    assert_eq!(raw(&fill, "D21").as_deref(), Some("Task 6"));

    let _clipboard = crate::app::tests::CLIP_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let paste = TestApp::<SheetsShell>::capture(Some("pasteformula")).unwrap();
    // E2's `=SUM(B2:D2)` pasted one row down reads its own row.
    assert_eq!(raw(&paste, "E3").as_deref(), Some("=SUM(B3:D3)"));
}

/// A reader kept up to date by what each change touched arrives where a
/// fresh read does, without reading the workbook whole.
#[test]
fn a_change_refreshes_only_what_it_touched() {
    let mut book = sample();
    let mut reader = tilo_resource::Reader::<Workbook>::read(book.doc()).unwrap();
    write(&mut book, &budget(), &[("B2", "1300")]);
    edit(
        &mut book,
        &Workbook::paths()
            .sheets
            .item(&budget())
            .name
            .set(Atom("Plan".into())),
    );
    assert!(reader.sync(book.doc()).unwrap());
    assert!(!reader.last_delta().is_everything(), "read in part");
    let fresh = Workbook::read(book.doc()).unwrap();
    assert_eq!(**reader.state(), fresh);
    let total = crate::cell_actions::shown_cells(&sheet_of(&fresh, &budget()).snapshot())
        .into_iter()
        .find(|cell| (cell.col, cell.row) == (4, 1))
        .unwrap();
    assert_eq!(total.display, "$3,700.00", "the edit is recalculated");
}

/// Search finds sheets and cells by what they hold and what they show,
/// and a hit opens the cell it is anchored at.
#[test]
fn search_hits_open_cells_the_workbook_resolves() {
    let state = sample().state().unwrap();
    let entries = Workbook::search(&state);
    let rent = entries
        .iter()
        .find(|entry| entry.title == "Budget!A2")
        .expect("a cell hit");
    assert!(rent.text.contains("Rent"));
    assert_eq!(
        rent.anchor.as_deref().and_then(parse_cell_anchor),
        Some((budget(), (0, 1)))
    );
    let total = entries
        .iter()
        .find(|entry| entry.title == "Budget!E2")
        .expect("a formula hit");
    assert!(total.text.contains("=SUM(B2:D2)") && total.text.contains("$3,600.00"));
    assert!(entries.iter().any(|entry| entry.title == "Sales"));
}

/// Deleting a formula range's first or last row/column shrinks it; only
/// deleting all of it yields `#REF!`.
#[test]
fn deleting_a_range_edge_row_or_column_shrinks_the_formula_range() {
    let mut book = Replica::<Workbook>::new();
    let sheet = Id::from_raw(FIRST_SHEET_ID);
    write(
        &mut book,
        &sheet,
        &[
            ("B2", "1"),
            ("B3", "2"),
            ("B4", "4"),
            ("C3", "8"),
            ("D1", "=SUM(B2:B4)"),
            ("E1", "=SUM($B3:C$3)"),
        ],
    );
    let delete_row = |book: &mut Replica<Workbook>, row: u32| {
        book.apply_confirmed(Workbook::DELETE_ROWS.with(verbs::DeleteRows {
            sheet: sheet.clone(),
            rows: vec![row],
        }))
        .unwrap();
    };
    delete_row(&mut book, 2);
    assert_eq!(raw(&book, &sheet, "D1"), "=SUM(B2:B3)");
    delete_row(&mut book, 3);
    assert_eq!(raw(&book, &sheet, "D1"), "=SUM(B2:B2)");

    assert_eq!(raw(&book, &sheet, "E1"), "=SUM($B2:C$2)");
    book.apply_confirmed(Workbook::DELETE_COLUMNS.with(verbs::DeleteColumns {
        sheet: sheet.clone(),
        columns: vec!["C".into()],
    }))
    .unwrap();
    assert_eq!(raw(&book, &sheet, "D1"), "=SUM($B2:B$2)");

    delete_row(&mut book, 2);
    assert_eq!(raw(&book, &sheet, "C1"), "=SUM(#REF!)");
}

/// Inserting rows moves cells and the formulas pointing at them; charts
/// and comments stay on their cells; a stable sheet id survives a reorder.
#[test]
fn rows_move_cells_formulas_and_anchors_and_targets_stay_put() {
    let mut book = sample();
    let budget = budget();
    let insert = Workbook::INSERT_ROWS.with(verbs::InsertRows {
        sheet: budget.clone(),
        at: 1,
        count: None,
    });
    book.apply(Workbook::ADD_CHART.with(verbs::AddChart {
        sheet: budget.clone(),
        range: "B2:E7".into(),
        kind: ChartKind::Column,
        title: "Spend".into(),
    }))
    .unwrap();
    book.apply(insert).unwrap();
    assert_eq!(raw(&book, &budget, "A2"), "Category");
    assert_eq!(raw(&book, &budget, "E3"), "=SUM(B3:D3)");
    let state = book.state().unwrap();
    let sheet = sheet_of(&state, &budget);
    let chart = &sheet.chart_snapshots(&sheet.layout())[0];
    assert_eq!(
        (chart.start, chart.end),
        ((1, 2), (4, 7)),
        "the chart moved down"
    );

    // Another person moved Budget after Sales: a write naming Budget's id
    // still lands on Budget.
    edit(&mut book, &Workbook::paths().sheets.move_to(&budget, 1));
    write(&mut book, &budget, &[("H8", "on the intended sheet")]);
    assert_eq!(raw(&book, &budget, "H8"), "on the intended sheet");
    let sales = Id::from_raw(crate::sample::SALES_SHEET_ID);
    assert_eq!(raw(&book, &sales, "H8"), "");
}

/// A workbook keeps one sheet; a duplicate is named after its original and
/// lands beside it.
#[test]
fn sheets_are_added_duplicated_and_the_last_is_kept() {
    let mut book = sample();
    book.apply(Workbook::DUPLICATE_SHEET.with(verbs::DuplicateSheet { sheet: budget() }))
        .unwrap();
    let names = |book: &Replica<Workbook>| -> Vec<String> {
        book.state()
            .unwrap()
            .sheets
            .values()
            .map(|sheet| sheet.name.0.clone())
            .collect()
    };
    assert_eq!(names(&book), ["Budget", "Budget copy", "Sales"]);
    let ids: Vec<Id> = book.state().unwrap().sheets.ids().cloned().collect();
    for id in &ids[..2] {
        book.apply_confirmed(Workbook::DELETE_SHEET.with(verbs::DeleteSheet { sheet: id.clone() }))
            .unwrap();
    }
    let last = Workbook::DELETE_SHEET.with(verbs::DeleteSheet {
        sheet: ids[2].clone(),
    });
    assert!(
        book.apply_confirmed(last).is_err(),
        "a workbook keeps one sheet"
    );
    book.apply(Workbook::NEW_SHEET.with(verbs::NewSheet {
        name: None,
        after: None,
    }))
    .unwrap();
    assert_eq!(names(&book), ["Sales", "Sheet2"]);
}

/// A batch with one bad cell writes nothing; bad names and colours are
/// refused at their paths.
#[test]
fn hostile_batches_are_refused_whole() {
    let mut book = sample();
    let heads = book.heads();
    let bad = Workbook::WRITE_CELLS.with(verbs::WriteCells {
        sheet: budget(),
        cells: vec![
            CellWrite::at((0, 30), "fine", None),
            CellWrite {
                cell: "XFE1".into(),
                input: Some("too far".into()),
                ..CellWrite::default()
            },
        ],
    });
    assert!(book.apply(bad).is_err());
    let huge = Workbook::WRITE_CELLS.with(verbs::WriteCells {
        sheet: budget(),
        cells: vec![CellWrite::at((0, 0), "x".repeat(40 * 1024), None)],
    });
    assert!(book.apply(huge).is_err(), "a cell holds at most 32 KiB");
    let sheet = Workbook::paths().sheets.item(&budget());
    assert!(
        book.apply(Invoke::edit(&sheet.name.set(Atom("  ".into()))).unwrap())
            .is_err()
    );
    assert!(
        book.apply(Invoke::edit(&sheet.color.set(Some(9))).unwrap())
            .is_err()
    );
    assert_eq!(book.heads(), heads, "nothing was written");
}

/// Two people editing different cells of one sheet, and reaching past it
/// at once, converge on one workbook with one row where both reached.
#[test]
fn two_replicas_converge() {
    let mut ours = sample();
    let mut theirs = ours.fork();
    write(&mut ours, &budget(), &[("B2", "1250"), ("A40", "ours")]);
    write(&mut theirs, &budget(), &[("C2", "1250"), ("B39", "theirs")]);
    ours.sync_with(&mut theirs).unwrap();
    let state = ours.state().unwrap();
    assert_eq!(state, theirs.state().unwrap());
    let snapshot = sheet_of(&state, &budget()).snapshot();
    assert_eq!(snapshot.raw.get(&(1, 1)).map(String::as_str), Some("1250"));
    assert_eq!(snapshot.raw.get(&(2, 1)).map(String::as_str), Some("1250"));
    assert_eq!(snapshot.raw.get(&(0, 39)).map(String::as_str), Some("ours"));
    assert_eq!(
        snapshot.raw.get(&(1, 38)).map(String::as_str),
        Some("theirs")
    );
    assert_eq!(
        sheet_of(&state, &budget()).layout().rows().len(),
        40,
        "the rows both added are one set"
    );
}

/// Two people writing the first cells of the same row at once both keep
/// their cell.
#[test]
fn first_writes_into_one_row_both_survive() {
    let mut ours = sample();
    let mut theirs = ours.fork();
    write(&mut ours, &budget(), &[("A40", "ours")]);
    write(&mut theirs, &budget(), &[("B40", "theirs")]);
    ours.sync_with(&mut theirs).unwrap();
    let snapshot = sheet_of(&ours.state().unwrap(), &budget()).snapshot();
    assert_eq!(snapshot.raw.get(&(0, 39)).map(String::as_str), Some("ours"));
    assert_eq!(
        snapshot.raw.get(&(1, 39)).map(String::as_str),
        Some("theirs")
    );
}

/// The selection's edits are the workbook's verbs with the selection
/// bound, beside its published actions.
#[test]
fn selection_edits_are_declarations_beside_the_published_workbook() {
    let doc = crate::sample::replica().doc().clone();
    let mut app = crate::app::tests::attached_app(&doc);
    ui::widget::Widget::sync(&mut app, &doc, &automerge::ROOT);
    app.select_range_for_test((0, 0), (1, 1));
    let decls = app.selection_declarations();
    let percent = decls
        .iter()
        .find(|decl| decl.id == "format-percent")
        .expect("format the selection");
    let built = percent
        .build_action(&tilo_actions::ActionArgs::default(), 0)
        .unwrap();
    let mut book = sample();
    book.apply_action(&built).unwrap();
    let state = book.state().unwrap();
    let formats = sheet_of(&state, &budget()).snapshot().fmt;
    for addr in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(formats.get(&addr), Some(&CellFormat::Percent), "{addr:?}");
    }
    let published = &app.published()[0];
    assert!(
        published
            .actions
            .iter()
            .any(|spec| spec.id == "format-percent")
    );
}

/// What a person or an agent changed reads as the app's sentences.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<SheetsShell>::new().unwrap();
    let sheet = app.state().sheets.ids().next().unwrap().clone();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let item = tilo_resource::items::item_resource_id::<Workbook>("sheets", &sheet);
    app.call_by(
        agent.clone(),
        &item,
        "write-cells",
        serde_json::json!({"cells": [{"cell": "A1", "input": "Total"}]}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        &item,
        "insert-rows",
        serde_json::json!({"at": 1}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        Workbook::KIND,
        "field-edit",
        serde_json::json!({
            "edit": Workbook::paths().sheets.item(&sheet).name.set(Atom("Plan".into()))
        }),
    )
    .unwrap();
    app.call_by(
        agent,
        &item,
        "comment",
        serde_json::json!({"range": "A2", "text": "Check this"}),
    )
    .unwrap();

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Wrote cells",
            "Inserted rows above row 1",
            "Renamed Sheet1 to Plan",
            "Commented on A2: Check this",
        ]
    );
}
