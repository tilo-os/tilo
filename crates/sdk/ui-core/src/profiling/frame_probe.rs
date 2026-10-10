//! Test-only attribution of real widget drawing, disabled outside the probe.
use crate::time::Instant;
use std::cell::RefCell;
use std::collections::BTreeMap;
type Rows = BTreeMap<&'static str, (u128, usize)>;
thread_local! {
    static ROWS: RefCell<Option<Rows>> = const { RefCell::new(None) };
}
pub(crate) fn enable() {
    ROWS.with_borrow_mut(|rows| *rows = Some(BTreeMap::new()));
}
pub(crate) fn take() -> BTreeMap<&'static str, (u128, usize)> {
    ROWS.with_borrow_mut(|rows| rows.take().unwrap_or_default())
}
pub(crate) struct Span {
    label: &'static str,
    start: Instant,
}
pub(crate) fn span(label: &'static str) -> Option<Span> {
    ROWS.with_borrow(|rows| rows.is_some()).then(|| Span {
        label,
        start: crate::time::now(),
    })
}
impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed().as_nanos();
        ROWS.with_borrow_mut(|rows| {
            if let Some(rows) = rows.as_mut() {
                let row = rows.entry(self.label).or_default();
                row.0 = row.0.saturating_add(elapsed);
                row.1 = row.1.saturating_add(1);
            }
        });
    }
}
