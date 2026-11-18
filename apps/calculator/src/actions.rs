//! What the calculator does beside changing its record: what the window shows,
//! recalling a past calculation, and the two graph lists. Only the evaluated
//! history and the saved graph formulas are in the resource; the expression being
//! typed, the display and the pan/zoom are window state.

use std::sync::atomic::{AtomicU64, Ordering};

use ui::tilo_resource::{self, Effect, Id};

/// What this window shows and recalls; nothing here is in the shared document.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "calculator.effect")]
pub enum CalculatorEffect {
    /// Show calculator.
    #[effect(view, idempotent)]
    ShowCalculator,
    /// Show 2D graph.
    #[effect(view, idempotent)]
    ShowGraph2d,
    /// Show 3D graph.
    #[effect(view, idempotent)]
    ShowGraph3d,
    /// Show unit conversion.
    #[effect(view, idempotent)]
    ShowConvert,
    /// Use degrees.
    #[effect(view, idempotent)]
    UseDegrees,
    /// Use radians.
    #[effect(view, idempotent)]
    UseRadians,
    /// Show or hide the graph's formula panel.
    #[effect(view)]
    TogglePanel,
    /// Recall calculation.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The history entry's id")]
        entry: String,
    },
    /// Use calculation.
    #[effect(view, idempotent)]
    Recall {
        #[param("The calculation to put back into the calculator")]
        expression: String,
    },
}

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
