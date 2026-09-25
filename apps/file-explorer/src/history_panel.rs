//! State for the file-history panel.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryPanel {
    pub(crate) record_id: String,
    pub(crate) path: PathBuf,
    /// Newest first. The filesystem projection is bounded, so this remains
    /// cheap to paint and safe to expose to accessibility clients.
    pub(crate) versions: Vec<crate::model::HistoryVersion>,
    pub(crate) heads: Vec<String>,
    pub(crate) selected: usize,
    pub(crate) compare: bool,
    pub(crate) selected_preview: Option<String>,
    pub(crate) current_preview: Option<String>,
    pub(crate) preview_loading: bool,
}
