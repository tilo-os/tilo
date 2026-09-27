//! Notes provider — walks every Notes-kind tab and matches the query
//! against tab title + Automerge body text; Enter focuses the tab. Walk,
//! scoring and body read are shared with the Editor (see [`super::text_app`]).

use crate::shell::chrome_state::ChromeState;
use crate::shell::notes_app::BODY_KEY;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

/// The `AppKind::Other` name a Notes tile carries.
const APP: &str = "notes";

pub struct NotesProvider {
    cache: super::text_app::TextSearchCache,
}

impl NotesProvider {
    pub fn new() -> Self {
        Self {
            cache: super::text_app::TextSearchCache::default(),
        }
    }
}

impl Default for NotesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for NotesProvider {
    fn id(&self) -> &'static str {
        "notes"
    }
    fn name(&self) -> &'static str {
        "Notes"
    }

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-notes")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Note]
    }
    fn owns_tab(&self, tab_id: u64, chrome: &ChromeState) -> bool {
        super::text_app::owns_tab_of(chrome, tab_id, APP)
    }
    fn proximity(
        &self,
        result: &SearchResult,
        chrome: &ChromeState,
    ) -> crate::shell::search::SearchProximity {
        super::text_app::proximity_of(chrome, result)
    }
    /// A title the query starts scores the maximum.
    fn max_score(&self) -> i32 {
        super::text_app::MAX_SCORE
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.query_page(ctx, None, SearchBudget::PALETTE).results
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        super::text_app::text_search_revision(ctx.chrome)
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        super::text_app::query_text_app_page(
            &self.cache,
            &ctx,
            APP,
            BODY_KEY,
            "notes",
            &ResourceKind::Note,
            cursor,
            budget,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::notes_app::NotesApp;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::collections::HashMap as StdMap;
    use std::sync::Arc;
    use ui::widget::Widget;

    fn tree_with_note_tab(tab_id: u64, title: &str) -> WorkspaceTreeHandle {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 4242;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: title.into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("Notes".into()),
                        label: title.into(),
                    },
                }],
                active_tab: tab_id,
                width: StateWidth::Normal,
                maximized: false,
            }],
        };
        let ws = Workspace::new(ws_id, "Home");
        let mut ws = ws;
        ws.strips = vec![strip];
        let mut active_strip = StdMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdMap::new();
        active_group.insert(strip_id, group_id);
        let tree = WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group,
                ..Default::default()
            },
            ..WorkspaceTree::default()
        };
        Arc::new(RwLock::new(tree))
    }

    /// Seed via [`NotesApp`], so the write lands where the provider reads.
    fn seed_note_body(chrome: &ChromeState, tab_id: u64, body: &str) {
        let record = crate::shell::tab_documents::scratch_tab_record(tab_id);
        let store = chrome.records();
        store.open_record(&record);
        store.with(&record.id, |doc| {
            let mut app = NotesApp::with_seed(body);
            app.sync(doc, &automerge::ROOT);
        });
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn notes_search_matches_title_then_body_of_notes_tabs() {
        // Phase `matches_title_over_body_by_score`.
        {
            let handle = tree_with_note_tab(1, "Grocery list");
            let chrome = ChromeState::new_with_handle(handle);
            let chrome_st = chrome.borrow();
            let doc = AutoCommit::new();
            seed_note_body(&chrome_st, 1, "reminder: nothing about grocery in body");
            let provider = NotesProvider::new();
            let out = provider.query(SearchContext {
                needle: "grocery",
                wanted: &[ResourceKind::Note],
                chrome: &chrome_st,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].title, "Grocery list");
            assert_eq!(
                out[0].score,
                crate::shell::search::providers::text_app::MAX_SCORE
            );
        }

        // Phase `matches_body_when_title_misses`.
        {
            let handle = tree_with_note_tab(1, "Untitled");
            let chrome = ChromeState::new_with_handle(handle);
            let chrome_st = chrome.borrow();
            let doc = AutoCommit::new();
            seed_note_body(&chrome_st, 1, "The quick brown fox jumps.");
            let provider = NotesProvider::new();
            let out = provider.query(SearchContext {
                needle: "quick brown",
                wanted: &[ResourceKind::Note],
                chrome: &chrome_st,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert_eq!(out.len(), 1);
            assert!(
                out[0]
                    .subtitle
                    .as_deref()
                    .unwrap_or("")
                    .contains("quick brown")
            );
        }

        // Phase `finds_a_word_that_only_appears_in_the_body_the_app_wrote`.
        // `tilo` matches nothing else about the tab, isolating a body-only match.
        {
            let handle = tree_with_note_tab(7, "Notes");
            let chrome = ChromeState::new_with_handle(handle);
            let chrome_st = chrome.borrow();
            let doc = AutoCommit::new();
            seed_note_body(
                &chrome_st,
                7,
                "Welcome to tilo.\n\nThese notes sync live with anyone \
                 invited to this workspace.",
            );
            let provider = NotesProvider::new();
            let out = provider.query(SearchContext {
                needle: "tilo",
                wanted: &[ResourceKind::Note],
                chrome: &chrome_st,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert_eq!(
                out.len(),
                1,
                "searching a note's own body must find it; got {:?}",
                out.iter().map(|r| (&r.title, r.score)).collect::<Vec<_>>()
            );
            assert!(
                out[0].score > 0 && out[0].score < 25,
                "a body hit ranks far below a title hit, got {}",
                out[0].score
            );
            let snippet = out[0].subtitle.as_deref().unwrap_or_default();
            assert!(
                snippet.contains("tilo"),
                "the row must show the matching line: {snippet:?}"
            );
            assert!(
                !snippet.contains("  ") && !snippet.contains('\n'),
                "the snippet must collapse the body's line structure: {snippet:?}"
            );
        }

        // Phase `no_hit_when_body_and_title_both_miss`.
        {
            let handle = tree_with_note_tab(1, "Untitled");
            let chrome = ChromeState::new_with_handle(handle);
            let chrome_st = chrome.borrow();
            let doc = AutoCommit::new();
            seed_note_body(&chrome_st, 1, "The quick brown fox jumps.");
            let provider = NotesProvider::new();
            let out = provider.query(SearchContext {
                needle: "xylophone",
                wanted: &[ResourceKind::Note],
                chrome: &chrome_st,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert!(out.is_empty());
        }

        // Phase `owns_tab_recognises_notes_kind`.
        {
            let handle = tree_with_note_tab(99, "n");
            let chrome = ChromeState::new_with_handle(handle);
            let chrome_st = chrome.borrow();
            let provider = NotesProvider::new();
            assert!(provider.owns_tab(99, &chrome_st));
            assert!(!provider.owns_tab(1234, &chrome_st));
        }
    }
}
