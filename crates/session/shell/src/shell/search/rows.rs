//! What a search row is *of*, and where a host looks for the row of a
//! resource itself.

use super::SearchResult;

/// The resource a row acts on: the one its provider named
/// ([`SearchResult::resource`]), else its own id, which a resource hit is
/// (`record.note.3`, `app.open`). How a host finds the row of a resource it
/// opened a search on ([`ui::search::SearchRequest::on`]).
///
/// [`ui::search::SearchRequest::on`]: ui::search::SearchRequest::on
#[must_use]
pub fn row_resource(row: &SearchResult) -> &str {
    row.resource.as_deref().unwrap_or(row.id.as_str())
}
