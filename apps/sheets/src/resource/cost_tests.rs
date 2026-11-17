//! What a large workbook costs to write, keep, open, read and recompute, and what one inserted
//! row costs on top: timings rather than a check, to be read in a release build.

use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::json;
use ui::tilo_resource::{Id, Invoke, Replica};

use super::{SheetSpec, Workbook, verbs};
use crate::calc::WorkbookCalc;
use crate::model::{Addr, CellValue};
use crate::workbook::{Cell, FIRST_SHEET_ID, cell_edits};

/// The least of three timings of `work`, with what it last answered: the machine these are
/// read on is never idle.
fn best<T>(mut work: impl FnMut() -> T) -> (Duration, T) {
    let mut timed = || {
        let at = Instant::now();
        let out = work();
        (at.elapsed(), out)
    };
    let (mut least, mut out) = timed();
    for _ in 0..2 {
        let (took, again) = timed();
        least = least.min(took);
        out = again;
    }
    (least, out)
}

/// The cells of rows `from..to`: a number and three formulas reading their own row.
fn rows_of(from: u32, to: u32) -> Vec<(Addr, Cell)> {
    (from..to)
        .flat_map(|row| {
            let r = row + 1;
            [
                ((0, row), Cell::of(r.to_string())),
                ((1, row), Cell::of(format!("=A{r}*2"))),
                ((2, row), Cell::of(format!("=B{r}+1"))),
                ((3, row), Cell::of(format!("=SUM(A{r}:C{r})"))),
            ]
        })
        .collect()
}

/// Write `cells` into the sheet as the grid does: one group of cell edits.
fn write(book: &mut Replica<Workbook>, sheet: &Id, cells: &[(Addr, Cell)]) {
    let state = book.live().unwrap().clone();
    let members: Vec<Invoke<Workbook>> = cell_edits(&state, sheet, cells)
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect();
    book.apply(Invoke::group(members)).unwrap();
}

/// The sheet read and recomputed after one cell was typed, three times over.
fn edits(
    book: &mut Replica<Workbook>,
    calc: &mut WorkbookCalc,
    sheet: &Id,
    cells: [Addr; 3],
) -> Duration {
    let mut least = Duration::MAX;
    for (cell, typed) in cells.into_iter().zip(["7", "8", "9"]) {
        write(book, sheet, &[(cell, Cell::of(typed))]);
        let state = Rc::new(book.live().unwrap().clone());
        let at = Instant::now();
        let read = state.snapshot_of(sheet).unwrap();
        calc.update(&state, None);
        least = least.min(at.elapsed());
        assert!(!read.raw.is_empty());
    }
    least
}

/// `cargo test --release -p sheets --lib -- --ignored --nocapture large_sheet_timings`
#[test]
#[ignore = "timings rather than a check, to be read in a release build"]
fn large_sheet_timings() {
    // A group of thousands of cell edits is past what an action keeps inline.
    let _content = ui::tilo_resource::content::register_for_resources();
    for rows in [10_000u32, 100_000] {
        let big = Id::from_raw(FIRST_SHEET_ID);
        let mut book = Replica::<Workbook>::new();
        // Every row and column at once, so the cells below are written into rows that exist.
        book.apply(Workbook::FORMAT_CELLS.with(verbs::FormatCells {
            sheet: big.clone(),
            range: format!("D{rows}"),
            format: None,
            bold: Some(true),
        }))
        .unwrap();

        let at = Instant::now();
        for from in (0..rows).step_by(2_500) {
            write(&mut book, &big, &rows_of(from, (from + 2_500).min(rows)));
        }
        let written = at.elapsed();
        book.apply(Workbook::NEW_SHEET.with(verbs::NewSheet {
            new: SheetSpec {
                name: Some("Small".to_owned()),
                rows: Some(vec![vec![
                    json!(format!("=Sheet1!D{rows}")),
                    json!("=SUM(Sheet1!A:A)"),
                ]]),
                ..SheetSpec::default()
            },
        }))
        .unwrap();

        let (saved, bytes) = best(|| book.save());
        let (open, state) = best(|| {
            let opened = Replica::<Workbook>::load(&bytes).unwrap();
            Rc::new(opened.state().unwrap())
        });
        let small = state.sheet_id(1).unwrap().clone();
        let (read, snapshot) = best(|| state.snapshot_of(&big).unwrap());
        let (computed, mut calc) = best(|| {
            let mut calc = WorkbookCalc::default();
            calc.update(&state, None);
            calc
        });
        let last = Some(CellValue::Number(f64::from(5 * rows + 1)));
        assert_eq!(calc.values(&small).get(&(0, 0)).cloned(), last);
        let edit = edits(&mut book, &mut calc, &big, [(0, 0), (0, 1), (0, 2)]);
        let kept = book.save().len();

        // One row inserted above everything: every formula below it reads one row lower.
        let insert = || {
            Workbook::INSERT_ROWS.with(verbs::InsertRows {
                sheet: big.clone(),
                at: 1,
                count: None,
            })
        };
        let mut inserted = Duration::MAX;
        for _ in 0..3 {
            let mut copy = book.fork();
            copy.live().unwrap();
            let at = Instant::now();
            copy.apply(insert()).unwrap();
            inserted = inserted.min(at.elapsed());
        }
        book.apply(insert()).unwrap();
        let (saved_moved, bytes_moved) = best(|| book.save());

        let state = Rc::new(book.live().unwrap().clone());
        let (read_moved, moved) = best(|| state.snapshot_of(&big).unwrap());
        assert_eq!(moved.raw.len(), snapshot.raw.len());
        assert_eq!(
            moved.raw.get(&(3, rows)).map(String::as_str),
            Some(format!("=SUM(A{0}:C{0})", rows + 1).as_str()),
            "the last row's formula reads its own row, one lower"
        );
        let at = Instant::now();
        calc.update(&state, None);
        let recomputed = at.elapsed();
        assert_eq!(calc.values(&small).get(&(0, 0)).cloned(), last);
        let (computed_moved, _) = best(|| {
            let mut calc = WorkbookCalc::default();
            calc.update(&state, None);
            calc
        });
        let edit_moved = edits(&mut book, &mut calc, &big, [(0, 1), (0, 2), (0, 3)]);
        let (open_moved, cells) = best(|| {
            let opened = Replica::<Workbook>::load(&bytes_moved).unwrap();
            opened.state().unwrap().snapshot_of(&big).unwrap().raw.len()
        });
        assert_eq!(cells, snapshot.raw.len());

        eprintln!(
            "rows={rows} written={written:?} saved={saved:?} bytes={} open={open:?} read={read:?} \
             computed={computed:?} edit={edit:?} | insert={inserted:?} synced={} \
             saved={saved_moved:?} read={read_moved:?} recomputed={recomputed:?} \
             computed={computed_moved:?} edit={edit_moved:?} open+read={open_moved:?}",
            bytes.len(),
            bytes_moved.len().abs_diff(kept),
        );
    }
}
