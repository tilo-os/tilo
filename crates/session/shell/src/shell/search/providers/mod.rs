//! Built-in [`SearchProvider`] implementations, one per submodule.
//!
//! [`SearchProvider`]: crate::shell::search::SearchProvider

pub mod actions;
pub mod applications;
mod background_index;
pub mod continuity;
pub mod desktop_windows;
pub mod files;
pub mod host_applications;
pub mod live_app_actions;
pub mod now_playing;
pub mod permission_prompts;
pub mod records;
pub mod session_actions;
pub mod shortcuts;
pub(crate) mod task_review;
pub mod terminal_device_shell;
pub mod tray_items;
pub mod unavailable_applications;

pub use actions::ActionsProvider;
pub use applications::{ApplicationsProvider, NativeApplicationsProvider};
pub use continuity::{
    CallContinuityActionsProvider, ContinueFromDeviceProvider, DeviceContinuityActionsProvider,
};
pub use desktop_windows::DesktopWindowsProvider;
pub use files::FilesProvider;
pub use host_applications::HostApplicationsProvider;
pub use live_app_actions::LiveAppActionsProvider;
pub use records::RecordsProvider;
pub use session_actions::{SessionActionPolicy, SessionActionsProvider};
pub use shortcuts::ShortcutsProvider;
pub use unavailable_applications::UnavailableApplicationsProvider;

use crate::shell::search::{SearchBudget, SearchCursor, SearchPage, SearchResult};

/// One page of a provider's ranked index hits, as rows, under `budget`.
///
/// The cursor token is `"{needle}\u{1f}{offset}"`, so a cursor handed out
/// for another revision or another query is stale rather than reinterpreted.
/// `search` runs the query from an offset; `row` turns a hit into its row, or
/// `None` for a hit the page passes over. A row costs the palette its id and
/// both labels plus a fixed overhead.
pub(super) fn page_hits(
    revision: String,
    needle: &str,
    cursor: Option<&SearchCursor>,
    budget: SearchBudget,
    search: impl FnOnce(usize) -> tilo_search::Hits,
    mut row: impl FnMut(&tilo_search::Hit) -> Option<SearchResult>,
) -> SearchPage {
    let offset = match cursor {
        None => 0,
        Some(cursor) => match cursor
            .token
            .split_once('\u{1f}')
            .filter(|(cursor_needle, _)| cursor.revision == revision && *cursor_needle == needle)
            .and_then(|(_, offset)| offset.parse::<usize>().ok())
        {
            Some(offset) => offset,
            None => return SearchPage::stale(revision),
        },
    };
    let found = search(offset);
    let mut bytes = 0usize;
    let mut consumed = 0usize;
    let mut stopped = false;
    let mut results = Vec::new();
    for hit in &found.hits {
        let Some(result) = row(hit) else {
            consumed = consumed.saturating_add(1);
            continue;
        };
        let cost = result
            .id
            .len()
            .saturating_add(result.title.len())
            .saturating_add(result.subtitle.as_ref().map_or(0, String::len))
            .saturating_add(96);
        if results.len() >= budget.max_results || bytes.saturating_add(cost) > budget.max_bytes {
            stopped = true;
            break;
        }
        bytes = bytes.saturating_add(cost);
        consumed = consumed.saturating_add(1);
        results.push(result);
    }
    let next = offset.saturating_add(consumed);
    SearchPage::continuing(
        revision,
        results,
        (stopped || found.more).then(|| format!("{needle}\u{1f}{next}")),
    )
}
