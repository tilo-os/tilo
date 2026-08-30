//! Notes as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Notes declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! page tree, so every note page becomes a typed [`SearchResult`] the unified
//! Ctrl+K palette can rank alongside every other provider's hits. Activating a
//! row hands back an opaque `action_id` the app resolves to a [`PageId`] and
//! navigates to.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! against page *titles* held in memory (`Workspace::pages`), never touching
//! disk or the network on the query path. Page bodies live as `.md` files on
//! disk (see [`crate::storage`]); reading every one on each keystroke would
//! violate the toolkit's no-per-frame-IO rule, so body text is intentionally
//! out of scope here.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{PageId, PageMeta, Workspace};

/// The kind label (and result-group header) Notes produces.
pub const NOTE_KIND: &str = "Notes";

/// The `action_id` prefix. A chosen row's `action_id` is `note:<uuid>`;
/// [`parse_action_id`] turns it back into a [`PageId`].
const ACTION_PREFIX: &str = "note:";

/// The title shown for a page whose title is still blank.
const UNTITLED: &str = "Untitled";

/// A [`SearchProvider`] over a snapshot of the page tree. Rebuild it (cheap —
/// it clones only the lightweight [`PageMeta`] list, not the bodies) whenever
/// the search surface opens.
pub struct NoteSearchProvider {
    pages: Vec<PageMeta>,
}

impl NoteSearchProvider {
    /// Snapshot the current page tree into a provider.
    pub fn new(workspace: &Workspace) -> Self {
        Self {
            pages: workspace.pages().to_vec(),
        }
    }

    /// The `action_id` for a page — what a chosen row hands back.
    pub fn action_id(id: PageId) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back to a [`PageId`],
    /// so the host can navigate to the chosen page. Returns `None` for an id
    /// this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<PageId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| PageId::parse_str(rest).ok())
    }

    /// The display title for a page, substituting a placeholder for a blank
    /// title so an untitled page is still findable and readable in the palette.
    fn display_title(page: &PageMeta) -> &str {
        if page.title.trim().is_empty() {
            UNTITLED
        } else {
            page.title.as_str()
        }
    }
}

impl SearchProvider for NoteSearchProvider {
    fn name(&self) -> &str {
        NOTE_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        // Substring filter over the (placeholder-substituted) title; the
        // registry re-ranks the survivors by `fuzzy_score` against the same
        // title. An empty query matches everything (the whole notebook is
        // browsable), matching the contacts reference.
        let q = query.to_lowercase();
        self.pages
            .iter()
            .filter(|p| Self::display_title(p).to_lowercase().contains(&q))
            .map(|p| {
                SearchResult::new(NOTE_KIND, Self::display_title(p), Self::action_id(p.id))
                    .with_icon(Icon::FileText)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Workspace;
    use ui::search::SearchRegistry;

    /// A workspace with three titled pages.
    fn populated() -> Workspace {
        let mut ws = Workspace::new();
        let a = ws.create_page(None);
        ws.rename(a, "Grocery list");
        let b = ws.create_page(None);
        ws.rename(b, "Meeting notes");
        let c = ws.create_page(None);
        ws.rename(c, "Trip planning");
        ws
    }

    #[test]
    fn provider_finds_a_real_page_by_title() {
        let ws = populated();
        let provider = NoteSearchProvider::new(&ws);
        let hits = provider.search("meeting");
        assert!(
            hits.iter().any(|r| r.title == "Meeting notes"),
            "the provider surfaces the matching page, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().all(|r| r.kind == NOTE_KIND),
            "all hits are tagged Notes"
        );
    }

    #[test]
    fn empty_query_browses_the_whole_notebook() {
        let ws = populated();
        let provider = NoteSearchProvider::new(&ws);
        assert_eq!(provider.search("").len(), ws.pages().len());
    }

    #[test]
    fn action_id_round_trips_to_the_chosen_page() {
        let ws = populated();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(NoteSearchProvider::new(&ws)));

        let groups = registry.query_grouped("trip");
        assert_eq!(groups[0].kind, NOTE_KIND);
        let top = &groups[0].results[0];
        assert_eq!(top.title, "Trip planning", "the best match leads");

        let id = NoteSearchProvider::parse_action_id(&top.action_id)
            .expect("action_id parses to a PageId");
        assert!(
            ws.get(id).is_some(),
            "the id resolves to a real stored page"
        );
        assert_eq!(
            ws.get(id).unwrap().title,
            top.title,
            "activation lands on the right page"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(NoteSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(NoteSearchProvider::parse_action_id("note:not-a-uuid").is_none());
    }

    #[test]
    fn untitled_page_is_still_findable() {
        let mut ws = Workspace::new();
        ws.create_page(None); // left untitled
        let provider = NoteSearchProvider::new(&ws);
        let hits = provider.search("untitled");
        assert_eq!(
            hits.len(),
            1,
            "a blank-titled page surfaces under its placeholder"
        );
    }
}
