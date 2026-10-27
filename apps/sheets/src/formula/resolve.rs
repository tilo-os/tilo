//! Evaluating a cell after the formulas it reads, from a stack on the heap.
//!
//! A column of formulas each reading the one above (a running total) is as
//! deep as the sheet is long. Evaluated by recursion alone it takes several
//! stack frames per row and overflows the thread's stack a few tens of
//! thousands of rows down, which aborts the app.

use std::collections::HashSet;
use std::hash::BuildHasher;

use super::{Engine, Expr, parse_in};
use crate::model::{Addr, CellValue, Col, FormulaError, Row};

/// A rectangle of cells a formula reads, its corners in order.
#[derive(Clone, Copy)]
struct Rect {
    c0: Col,
    c1: Col,
    r0: Row,
    r1: Row,
}

impl Rect {
    fn spanning(a: Addr, b: Addr) -> Self {
        Self {
            c0: a.0.min(b.0),
            c1: a.0.max(b.0),
            r0: a.1.min(b.1),
            r1: a.1.max(b.1),
        }
    }

    fn area(self) -> u64 {
        u64::from(self.c0.abs_diff(self.c1))
            .saturating_add(1)
            .saturating_mul(u64::from(self.r0.abs_diff(self.r1)).saturating_add(1))
    }

    fn contains(self, (c, r): Addr) -> bool {
        (self.c0..=self.c1).contains(&c) && (self.r0..=self.r1).contains(&r)
    }

    /// The cell after `at`, row by row, or `None` past the last.
    fn after(self, (c, r): Addr) -> Option<Addr> {
        if c < self.c1 {
            Some((c.saturating_add(1), r))
        } else if r < self.r1 {
            Some((self.c0, r.saturating_add(1)))
        } else {
            None
        }
    }
}

/// A formula [`Engine::resolve`] has met and not yet evaluated: its parse,
/// and how far it has looked through the cells it reads. It keeps its place
/// in a range rather than a list of the range, so a column of formulas each
/// reading every row above (`=MAX($A$1:A1)+1` numbering rows) holds memory
/// in proportion to the column, not to its square.
struct Pending {
    addr: Addr,
    parsed: Result<Expr, FormulaError>,
    /// The rectangles it reads that it has not started on.
    rects: Vec<Rect>,
    /// The rectangle it is walking, and the next cell to look at there.
    walk: Option<(Rect, Addr)>,
    /// The formula cells of a rectangle larger than the populated cells,
    /// not yet looked at.
    listed: Vec<Addr>,
}

/// Every cell or range `expr` reads, as rectangles.
fn referenced_rects(expr: &Expr, out: &mut Vec<Rect>) {
    match expr {
        Expr::Ref(addr) => out.push(Rect::spanning(*addr, *addr)),
        Expr::Range(a, b) => out.push(Rect::spanning(*a, *b)),
        Expr::Unary(_, inner) => referenced_rects(inner, out),
        Expr::Binary(_, l, r) => {
            referenced_rects(l, out);
            referenced_rects(r, out);
        }
        Expr::Call(_, args) => {
            for arg in args {
                referenced_rects(arg, out);
            }
        }
        Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Missing => {}
    }
}

impl<S: BuildHasher> Engine<'_, S> {
    /// The value of the cell at `addr`, every formula it reads evaluated
    /// first, deepest first, from a stack on the heap: each formula is
    /// evaluated once the formulas it reads have values, so evaluating it
    /// recurses only into cached cells. A formula on a cycle still meets its
    /// own cell through [`Engine::eval_cell`] and reads as a cycle.
    pub(super) fn resolve(&mut self, addr: Addr) -> CellValue {
        let mut stack = Vec::new();
        let mut open = HashSet::new();
        self.open_formula(addr, &mut stack, &mut open);
        while let Some(top) = stack.last_mut() {
            if let Some(read) = self.next_read(top, &open) {
                self.open_formula(read, &mut stack, &mut open);
                continue;
            }
            let Some(done) = stack.pop() else {
                break;
            };
            open.remove(&done.addr);
            // A cycle through this cell may have evaluated it already.
            if self.cache.contains_key(&done.addr) {
                continue;
            }
            self.visiting.insert(done.addr);
            let value = match &done.parsed {
                Ok(expr) => self.eval_expr(expr),
                Err(error) => CellValue::Error(*error),
            };
            self.visiting.remove(&done.addr);
            self.cache.insert(done.addr, value);
        }
        self.eval_cell(addr)
    }

    /// Put the formula at `addr` on `stack`; a cell holding no formula needs
    /// no stack and is left alone.
    fn open_formula(&self, addr: Addr, stack: &mut Vec<Pending>, open: &mut HashSet<Addr>) {
        let Some(body) = self.raw.get(&addr).and_then(|raw| raw.strip_prefix('=')) else {
            return;
        };
        let parsed = parse_in(body, self.place_of(addr)).map_err(|e| e.error);
        let mut rects = Vec::new();
        if let Ok(expr) = &parsed {
            referenced_rects(expr, &mut rects);
        }
        open.insert(addr);
        stack.push(Pending {
            addr,
            parsed,
            rects,
            walk: None,
            listed: Vec::new(),
        });
    }

    /// The next formula cell `pending` reads that has no value yet and is
    /// not open below it, walking whichever is smaller for each rectangle:
    /// the rectangle or the populated cells.
    fn next_read(&self, pending: &mut Pending, open: &HashSet<Addr>) -> Option<Addr> {
        let waiting = |addr: Addr| {
            self.raw.get(&addr).is_some_and(|raw| raw.starts_with('='))
                && !self.cache.contains_key(&addr)
                && !open.contains(&addr)
        };
        loop {
            if let Some(addr) = pending.listed.pop() {
                if waiting(addr) {
                    return Some(addr);
                }
                continue;
            }
            if let Some((rect, at)) = pending.walk {
                pending.walk = rect.after(at).map(|next| (rect, next));
                if waiting(at) {
                    return Some(at);
                }
                continue;
            }
            let rect = pending.rects.pop()?;
            if rect.area() > u64::try_from(self.raw.len()).unwrap_or(u64::MAX) {
                pending.listed.extend(
                    self.raw
                        .keys()
                        .filter(|addr| rect.contains(**addr))
                        .filter(|addr| waiting(**addr))
                        .copied(),
                );
            } else {
                pending.walk = Some((rect, (rect.c0, rect.r0)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tilo_test_env::allocation;

    use super::super::{Engine, recalculate};
    use crate::model::CellValue;

    // Meters allocations only on a thread measuring them.
    #[global_allocator]
    static ALLOCATOR: allocation::Meter = allocation::Meter;

    /// A running total down a long column (`A2` is `=A1+1`, and so on a hundred
    /// thousand rows deep) evaluates on a thread with the stack a desktop
    /// main thread has, instead of recursing once per row until the stack
    /// overflows and the app aborts.
    #[test]
    fn a_long_running_total_evaluates_without_exhausting_the_stack() {
        const ROWS: u32 = 100_000;
        const LAST: u32 = ROWS - 1;
        // What a desktop session's main thread gets (`ulimit -s`).
        const MAIN_THREAD_STACK: usize = 8 << 20;
        let mut raw = HashMap::new();
        raw.insert((0, 0), "1".to_owned());
        for row in 1..ROWS {
            raw.insert((0, row), format!("=A{row}+1"));
        }
        let last = std::thread::Builder::new()
            .stack_size(MAIN_THREAD_STACK)
            .spawn(move || recalculate(&raw).remove(&(0, LAST)))
            .expect("spawn the evaluating thread")
            .join()
            .expect("the evaluating thread finishes");
        assert_eq!(last, Some(CellValue::Number(f64::from(ROWS))));
    }

    /// A column numbering its rows with `=MAX($A$1:A1)+1`, each row reading
    /// every row above, is worked out holding memory in proportion to the
    /// column. Listing every range up front held one list per open formula,
    /// the column's square in all: tens of megabytes for these three thousand
    /// rows, gigabytes for a sheet ten times as long.
    #[test]
    fn a_column_reading_every_row_above_holds_memory_in_proportion_to_it() {
        const ROWS: u32 = 3_000;
        const LAST: u32 = ROWS - 1;
        let mut raw = HashMap::new();
        raw.insert((0, 0), "0".to_owned());
        for row in 1..ROWS {
            raw.insert((0, row), format!("=MAX($A$1:A{row})+1"));
        }
        let (last, held) = allocation::peak(|| Engine::new(&raw).resolve((0, LAST)));
        assert_eq!(last, CellValue::Number(f64::from(LAST)));
        // The value of every row and a few open formulas: well under 1 KiB a row.
        let bound = usize::try_from(ROWS).expect("a row count fits usize") << 10;
        assert!(held < bound, "held {held} bytes working out {ROWS} rows");
    }
}
