//! Evaluating a cell after the formulas it reads, from a stack on the heap.
//!
//! A column of formulas each reading the one above (a running total) is as
//! deep as the sheet is long. Evaluated by recursion alone it takes several
//! stack frames per row and overflows the thread's stack a few tens of
//! thousands of rows down, which aborts the app.

use std::collections::HashSet;
use std::hash::BuildHasher;

use super::{Engine, Expr, parse_in};
use crate::model::{Addr, CellValue, FormulaError};

/// A formula [`Engine::resolve`] has met and not yet evaluated: its parse,
/// and the formulas it reads still to evaluate before it.
struct Pending {
    addr: Addr,
    parsed: Result<Expr, FormulaError>,
    reads: Vec<Addr>,
}

/// Every cell or range `expr` reads, as (corner, corner) rectangles.
fn referenced_rects(expr: &Expr, out: &mut Vec<(Addr, Addr)>) {
    match expr {
        Expr::Ref(addr) => out.push((*addr, *addr)),
        Expr::Range(a, b) => out.push((*a, *b)),
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
            if let Some(read) = top.reads.pop() {
                if !open.contains(&read) && !self.cache.contains_key(&read) {
                    self.open_formula(read, &mut stack, &mut open);
                }
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

    /// Put the formula at `addr` on `stack` with the uncached formulas it
    /// reads; a cell holding no formula needs no stack and is left alone.
    fn open_formula(&self, addr: Addr, stack: &mut Vec<Pending>, open: &mut HashSet<Addr>) {
        let Some(body) = self.raw.get(&addr).and_then(|raw| raw.strip_prefix('=')) else {
            return;
        };
        let parsed = parse_in(body, self.place_of(addr)).map_err(|e| e.error);
        let mut reads = Vec::new();
        if let Ok(expr) = &parsed {
            let mut rects = Vec::new();
            referenced_rects(expr, &mut rects);
            for (a, b) in rects {
                self.uncached_formulas_within(a, b, &mut reads);
            }
        }
        open.insert(addr);
        stack.push(Pending {
            addr,
            parsed,
            reads,
        });
    }

    /// The formula cells inside the rectangle `a`..`b` with no value yet,
    /// walking whichever is smaller: the rectangle or the populated cells.
    fn uncached_formulas_within(&self, a: Addr, b: Addr, out: &mut Vec<Addr>) {
        let (c0, c1) = (a.0.min(b.0), a.0.max(b.0));
        let (r0, r1) = (a.1.min(b.1), a.1.max(b.1));
        let pending = |addr: &Addr| {
            !self.cache.contains_key(addr)
                && self.raw.get(addr).is_some_and(|raw| raw.starts_with('='))
        };
        let area = u64::from(c0.abs_diff(c1))
            .saturating_add(1)
            .saturating_mul(u64::from(r0.abs_diff(r1)).saturating_add(1));
        if area > u64::try_from(self.raw.len()).unwrap_or(u64::MAX) {
            out.extend(
                self.raw
                    .keys()
                    .filter(|(c, r)| (c0..=c1).contains(c) && (r0..=r1).contains(r))
                    .filter(|addr| pending(addr))
                    .copied(),
            );
        } else {
            for r in r0..=r1 {
                out.extend((c0..=c1).map(|c| (c, r)).filter(|addr| pending(addr)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::recalculate;
    use crate::model::CellValue;

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
}
