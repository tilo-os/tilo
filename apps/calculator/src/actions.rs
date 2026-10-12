//! What the calculator does beside changing its record: which part of it
//! is shown and recalling a past calculation ([`CalculatorEffect`]), and the
//! two graph lists ([`GraphKind`]).
//!
//! Most of a calculator is ephemeral: the expression being typed, the running
//! display, which mode (Calc / 2D / 3D) is showing, the pan/zoom of a graph, the
//! angle-unit strip, the memory scratch register. None of that is in the
//! record. What is — the evaluated history and the saved graph formulas — is
//! the [`Calculator`] model: formulas and visibility are field edits at their
//! paths, committing, deleting and clearing are its verbs
//! ([`crate::resource`]).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use ui::tilo_resource::{self, Collection, Effect, Id, Path};

use crate::model::PlotFn;
use crate::resource::Calculator;

/// Which part of the calculator is shown, and recalling a past calculation:
/// what this window shows, not the shared document.
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
    /// Recall calculation.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The history entry's id")]
        entry: String,
    },
}

/// The most recent evaluated computations kept in the shared history list.
/// The front is the newest; overflow is dropped from the tail.
pub const HISTORY_CAP: usize = 64;

/// Which saved-graph list a row is in. The 2D curves and the 3D surfaces are
/// stored identically, as two collections of [`PlotFn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphKind {
    /// The 2D `y = f(x)` curves.
    Curve2d,
    /// The 3D `z = f(x, y)` surfaces.
    Surface3d,
}

impl GraphKind {
    /// The collection field this kind is stored in.
    pub fn field(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "funcs",
            GraphKind::Surface3d => "surfaces",
        }
    }

    /// The path of this kind's rows.
    pub fn rows(self) -> Path<Calculator, Collection<PlotFn>> {
        let paths = Calculator::paths();
        match self {
            GraphKind::Curve2d => paths.funcs,
            GraphKind::Surface3d => paths.surfaces,
        }
    }

    /// A stable slug for the ids of rows this window adds.
    fn slug(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "curve2d",
            GraphKind::Surface3d => "surface3d",
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh id for a graph row this window adds. Identity is chosen before
/// the edit is encoded, never during replay: the timestamp makes
/// independently running collaborator windows disjoint; the process counter
/// also distinguishes two additions in the same tick.
pub fn new_row_id(kind: GraphKind) -> Id {
    let tick = ui::time::unix_now_ns();
    Id::from_raw(format!(
        "{}-{tick}-{}",
        kind.slug(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
