//! History policy for the retained live terminal session.

use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

pub const LIMITATION: &str = "Terminal History cannot reconstruct or restore a live process, its PTY, or process-produced screen output.";

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.terminal",
    scopes: &[HistoryScopeDefinition {
        key: "process-output",
        title: "Process output (not recorded)",
        presentation: HistoryPresentation::External { reason: LIMITATION },
        content_schema: None,
    }],
};
