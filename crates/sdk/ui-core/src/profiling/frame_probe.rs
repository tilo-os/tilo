//! Test-only attribution of real widget drawing, disabled outside the probe.
#[cfg(not(test))]
use crate::time::Instant;
// A test may pin the app's clock (`crate::time::testing`): spans read the
// process clock, so they still measure what the work cost.
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(test)]
use std::time::Instant;
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
pub(crate) fn enabled() -> bool {
    ROWS.with_borrow(Option::is_some)
}
/// Attributes `nanos` measured elsewhere to `label`.
pub(crate) fn record(label: &'static str, nanos: u128) {
    ROWS.with_borrow_mut(|rows| {
        if let Some(rows) = rows.as_mut() {
            let row = rows.entry(label).or_default();
            row.0 = row.0.saturating_add(nanos);
            row.1 = row.1.saturating_add(1);
        }
    });
}
pub(crate) struct Span {
    label: &'static str,
    start: Instant,
}
pub(crate) fn span(label: &'static str) -> Option<Span> {
    ROWS.with_borrow(|rows| rows.is_some()).then(|| Span {
        label,
        start: now(),
    })
}
#[cfg(not(test))]
fn now() -> Instant {
    crate::time::now()
}
#[cfg(test)]
fn now() -> Instant {
    Instant::now()
}
impl Drop for Span {
    fn drop(&mut self) {
        record(self.label, self.start.elapsed().as_nanos());
    }
}
