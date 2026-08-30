//! Editor provider — the Editor tab's own document, searchable from
//! `Ctrl+K` while it is the focused app. The walk, scoring and body read
//! are the shared ones in [`super::text_app`], so it can't drift from Notes.

use crate::shell::chrome_state::ChromeState;
use crate::shell::editor_app::BODY_KEY;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// The `AppKind::Other` name an Editor tile carries.
const APP: &str = "editor";

/// Rides [`ResourceKind::Note`]; the provider id tells the two apart where
/// it matters (`SearchScope::ActiveApp` narrows by provider, not kind).
const KIND: ResourceKind = ResourceKind::Note;

pub struct EditorProvider;

impl EditorProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for EditorProvider {
    fn id(&self) -> &'static str {
        "editor"
    }
    fn name(&self) -> &'static str {
        "Editor"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[KIND]
    }
    fn owns_tab(&self, tab_id: u64, chrome: &ChromeState) -> bool {
        super::text_app::owns_tab_of(chrome, tab_id, APP)
    }
    /// A title prefix scores 110 - see [`super::text_app::match_score`].
    fn max_score(&self) -> i32 {
        super::text_app::MAX_SCORE
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        super::text_app::query_text_app(&ctx, APP, BODY_KEY, "editor", &KIND)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::editor_app::EditorApp;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::collections::HashMap as StdMap;
    use std::sync::Arc;
    use ui::widget::Widget;

    fn tree_with_editor_tab(tab_id: u64, title: &str) -> WorkspaceTreeHandle {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 77;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: title.into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("Editor".into()),
                        label: title.into(),
                    },
                }],
                active_tab: tab_id,
                width: StateWidth::Normal,
                maximized: false,
            }],
        };
        let mut ws = Workspace::new(ws_id, "Home");
        ws.strips = vec![strip];
        let mut active_strip = StdMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdMap::new();
        active_group.insert(strip_id, group_id);
        Arc::new(RwLock::new(WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        }))
    }

    /// Seed a tab's body into its own record, where a hosted app writes.
    fn seed_tab_body(chrome: &ChromeState, tab_id: u64, seed: impl FnOnce(&mut AutoCommit)) {
        let record = crate::shell::tab_documents::scratch_tab_record(tab_id);
        let store = chrome.records();
        store.open_record(&record);
        store.with(&record.id, seed);
    }

    /// Reads from the Editor's own key, not the note's.
    #[test]
    fn finds_a_word_that_only_appears_in_the_editor_document() {
        let handle = tree_with_editor_tab(3, "Editor");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let doc = AutoCommit::new();
        seed_tab_body(&chrome_st, 3, |d| {
            let mut app = EditorApp::with_seed("export function renderScene() {}");
            app.sync(d, &automerge::ROOT);
        });

        let provider = EditorProvider::new();
        let out = provider.query(SearchContext {
            needle: "renderscene",
            wanted: &[KIND],
            chrome: &chrome_st,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert_eq!(
            out.len(),
            1,
            "searching the editor's own document must find it; got {out:?}"
        );
        assert_eq!(out[0].score, 40, "a body hit scores 40");
        assert!(
            out[0]
                .subtitle
                .as_deref()
                .unwrap_or_default()
                .contains("renderScene"),
            "the row must show the matching source line: {:?}",
            out[0].subtitle
        );
    }

    #[test]
    fn does_not_read_the_notes_body() {
        let handle = tree_with_editor_tab(3, "Editor");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let doc = AutoCommit::new();
        seed_tab_body(&chrome_st, 3, |d| {
            let mut notes = crate::shell::notes_app::NotesApp::with_seed("xylophone");
            notes.sync(d, &automerge::ROOT);
        });

        let out = EditorProvider::new().query(SearchContext {
            needle: "xylophone",
            wanted: &[KIND],
            chrome: &chrome_st,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(
            out.is_empty(),
            "the editor provider answered with the note's body: {out:?}"
        );
    }

    #[test]
    fn owns_only_editor_tabs() {
        let handle = tree_with_editor_tab(42, "Editor");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let provider = EditorProvider::new();
        assert!(provider.owns_tab(42, &chrome_st));
        assert!(!provider.owns_tab(43, &chrome_st));
        // …and the Notes provider must not claim an Editor tab.
        assert!(!super::super::notes::NotesProvider::new().owns_tab(42, &chrome_st));
    }
}
