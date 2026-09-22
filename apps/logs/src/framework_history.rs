//! History policy for Logs' external journal observations and local queries.

use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

pub const LIMITATION: &str = "Logs journal records, queries, selection, follow state, and exports are external or local state and are not recorded as document versions. Restore is unavailable.";

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.logs",
    scopes: &[HistoryScopeDefinition {
        key: "journal-observations",
        title: "Journal observations (not recorded)",
        presentation: HistoryPresentation::External { reason: LIMITATION },
        content_schema: None,
    }],
};
