//! History policy for Logs' external journal observations and local queries.

use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

pub const LIMITATION: &str =
    "The journal comes from the system, so History keeps no versions of it to restore.";

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.logs",
    scopes: &[HistoryScopeDefinition {
        key: "journal-observations",
        title: "System journal",
        presentation: HistoryPresentation::External { reason: LIMITATION },
        content_schema: None,
    }],
};
