//! The index a local action row is found by: its label, the words its app
//! declares for it, what it acts on and, for machines, its ids.

use tilo_search::{Document, Matching, SearchIndex};
use ui::search::FieldWeight;

use super::{PublishedAction, PublishedSurface};

/// Index the fields a local action row ranks by, keyed by position.
pub(super) fn index_actions(
    entries: &[(PublishedSurface, PublishedAction)],
    include_programmatic: bool,
) -> SearchIndex {
    let mut index = SearchIndex::new(Matching {
        spanning: true,
        typos: false,
    })
    .with_ranks(ui::search::system::action_ranks);
    index.replace_all(
        entries
            .iter()
            .enumerate()
            .filter(|(_, (_, action))| include_programmatic || action.contract.reaches_people())
            .map(|(at, (surface, action))| {
                // The words the app declares for the action find it as the
                // shell's own actions are found by theirs.
                action.keywords.iter().fold(
                    Document::new(at as u64)
                        .field(action.action_label.as_str(), FieldWeight::TITLE)
                        .field(action.resource_label.as_str(), FieldWeight::SUBTITLE)
                        .field(action.action.as_str(), FieldWeight::IDENTITY)
                        .field(action.resource.as_str(), FieldWeight::IDENTITY)
                        .field(surface.app_id.as_str(), FieldWeight::METADATA),
                    |document, word| document.field(word.as_str(), FieldWeight::KEYWORD),
                )
            }),
    );
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index live app actions");
    }
    index
}
