//! History policy for media sources played by Video Player.

use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

pub const LIMITATION: &str = "Video Player does not retain versions of media or an edit journal; recent media is device-local viewing history. Restore is unavailable.";

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.video-player",
    scopes: &[HistoryScopeDefinition {
        key: "media-versions",
        title: "Media versions (not recorded)",
        presentation: HistoryPresentation::External { reason: LIMITATION },
        content_schema: None,
    }],
};
