//! Notes provider — walks every Notes-kind tab across every
//! workspace/strip/group and matches the query against tab title +
//! Automerge body text.
//!
//! Enter on a result focuses the tab (switches workspace, strip, group,
//! and active tab pointer). No side-thread — notes live in the same
//! Automerge doc the palette callback already gets, so the match runs
//! synchronously against `chrome.tree()`.
//!
//! The walk, the scoring and the body read are shared with the Editor
//! (see [`super::text_app`]); this module is the Notes flavour of it.

use crate::shell::chrome_state::ChromeState;
use crate::shell::notes_app::BODY_KEY;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// The `AppKind::Other` name a Notes tile carries.
const APP: &str = "notes";

pub struct NotesProvider;

impl NotesProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NotesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for NotesProvider {
    fn id(&self) -> &str {
        "notes"
    }
    fn name(&self) -> &str {
        "Notes"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Note]
    }
    fn owns_tab(&self, tab_id: u64, chrome: &ChromeState) -> bool {
        super::text_app::owns_tab_of(chrome, tab_id, APP)
    }
    /// A title prefix scores 110 - see [`super::text_app::match_score`].
    fn max_score(&self) -> i32 {
        super::text_app::MAX_SCORE
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        super::text_app::query_text_app(&ctx, APP, BODY_KEY, "notes", ResourceKind::Note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::notes_app::NotesApp;
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
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        };
        Arc::new(RwLock::new(tree))
    }

    /// Seed the body the way the *app* does: construct the real
    /// [`NotesApp`] and let its `sync` write into the document the tab
    /// holds — its own record, which is where a hosted Notes tab's body
    /// lives. A hand-rolled seed, or one written to some other document,
    /// is what let the provider read from a path nothing ever wrote to.
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
    fn matches_title_over_body_by_score() {
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
        // Prefix on title (110) — the body-only match would score 40.
        assert_eq!(out[0].score, 110);
    }

    #[test]
    fn matches_body_when_title_misses() {
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

    /// The reported bug in the words it was reported in, against the note
    /// body that was reported with. (The shell no longer seeds that text
    /// on first boot — a note opens empty — so the body is written here
    /// rather than assumed.) `nir` matches nothing else about the tab —
    /// not its title, not its label. The
    /// provider looked for the body under a per-tab map keyed by the tab
    /// id, a shape the tile wiring never creates, so this found nothing.
    #[test]
    fn finds_a_word_that_only_appears_in_the_body_the_app_wrote() {
        let handle = tree_with_note_tab(7, "Notes");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let doc = AutoCommit::new();
        seed_note_body(
            &chrome_st,
            7,
            "Welcome to nirios.\n\nThese notes sync live with anyone \
             invited to this workspace.",
        );
        let provider = NotesProvider::new();
        let out = provider.query(SearchContext {
            needle: "nir",
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
        assert_eq!(out[0].score, 40, "a body hit scores 40");
        let snippet = out[0].subtitle.as_deref().unwrap_or_default();
        assert!(
            snippet.contains("nirios"),
            "the row must show the matching line: {snippet:?}"
        );
        // One line of prose, not the body's own line breaks: the seed has
        // a paragraph break right after the match, and two spaces in the
        // middle of a palette row read as a typo.
        assert!(
            !snippet.contains("  ") && !snippet.contains('\n'),
            "the snippet must collapse the body's line structure: {snippet:?}"
        );
    }

    #[test]
    fn no_hit_when_body_and_title_both_miss() {
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

    #[test]
    fn owns_tab_recognises_notes_kind() {
        let handle = tree_with_note_tab(99, "n");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let provider = NotesProvider::new();
        assert!(provider.owns_tab(99, &chrome_st));
        assert!(!provider.owns_tab(1234, &chrome_st));
    }
}
