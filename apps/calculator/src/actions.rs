//! What the calculator does beside changing its record: what the window shows,
//! recalling a past calculation, and the two graph lists. The vocabulary is
//! [`calculator_model::CalculatorEffect`], published by the resource; the window
//! answers it.

use std::sync::atomic::{AtomicU64, Ordering};

use ui::tilo_resource::Id;

/// What this window shows and recalls; nothing here is in the shared document.
pub use calculator_model::CalculatorEffect;

pub use calculator_model::{GraphKind, HISTORY_CAP};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Chosen before the edit is encoded, never during replay: the timestamp
/// separates windows, the counter separates two additions in one tick.
pub fn new_row_id(kind: GraphKind) -> Id {
    let tick = ui::time::unix_now_ns();
    Id::from_raw(format!(
        "{}-{tick}-{}",
        kind.slug(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
