//! History policy for Monitor's live system observations.

use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

pub const LIMITATION: &str = "Monitor process lists, metrics, charts, signals, and prior machine state are live system observations and are not recorded as document versions. Restore is unavailable.";

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.monitor",
    scopes: &[HistoryScopeDefinition {
        key: "system-observations",
        title: "System activity",
        presentation: HistoryPresentation::External { reason: LIMITATION },
        content_schema: None,
    }],
};
