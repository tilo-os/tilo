//! Notes provider — walks every Notes-kind tab across every
//! workspace/strip/group and matches the query against tab title +
//! Automerge body text.
//!
//! Enter on a result focuses the tab (switches workspace, strip, group,
//! and active tab pointer). No side-thread — notes live in the same
//! Automerge doc the palette callback already gets, so the match runs
//! synchronously against `chrome.tree()`.

use std::rc::Rc;

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::notes_app::BODY_KEY;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{AppKind, GroupId, StripId, TabId, TileContent, WorkspaceId};

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
        let tree = chrome.tree();
        for w in &tree.workspaces {
            for s in &w.strips {
                for g in &s.window_groups {
                    for t in &g.tabs {
                        if t.id == tab_id {
                            return matches!(tile_notes_kind(&t.tile), Some(true));
                        }
                    }
                }
            }
        }
        false
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Note) {
            return Vec::new();
        }
        let needle_lower = ctx.needle.to_lowercase();
        let mut out = Vec::new();
        let tree = ctx.chrome.tree();
        for w in &tree.workspaces {
            for s in &w.strips {
                for g in &s.window_groups {
                    for t in &g.tabs {
                        if tile_notes_kind(&t.tile) != Some(true) {
                            continue;
                        }
                        let title_lower = t.title.to_lowercase();
                        let body = read_note_body(ctx.doc, ctx.doc_root, t.id);
                        let body_lower = body.to_lowercase();
                        let Some((score, snippet)) =
                            note_match_score(&needle_lower, &title_lower, &body_lower, &body)
                        else {
                            continue;
                        };
                        let workspace_id = w.id;
                        let strip_id = s.id;
                        let group_id = g.id;
                        let tab_id = t.id;
                        let title = t.title.clone();
                        out.push(SearchResult {
                            kind: ResourceKind::Note,
                            provider_id: "notes".to_string(),
                            id: format!("{}", tab_id),
                            title: title.clone(),
                            subtitle: snippet,
                            score,
                            on_open: Rc::new(move |_doc, _root, chrome: &Chrome| {
                                focus_note_tab(chrome, workspace_id, strip_id, group_id, tab_id);
                            }),
                        });
                    }
                }
            }
        }
        drop(tree);
        out
    }
}

fn tile_notes_kind(tile: &TileContent) -> Option<bool> {
    match tile {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            Some(matches!(
                kind,
                AppKind::Other(name) if name.eq_ignore_ascii_case("notes")
            ))
        }
        _ => None,
    }
}

/// Notes tabs stash their body under a per-tab map at the doc root keyed
/// by the tab's stringified id (see the shell's tile-binding wiring).
/// The map has one field `BODY_KEY` holding an Automerge `Text` object.
/// Missing tabs / non-map entries / non-text bodies all return an empty
/// string so the search just falls back to title-only matching.
fn read_note_body(doc: &AutoCommit, doc_root: &ObjId, tab_id: TabId) -> String {
    let key = tab_id.to_string();
    let Ok(Some((value, obj))) = doc.get(doc_root, &key) else {
        return String::new();
    };
    if !matches!(value, Value::Object(ObjType::Map)) {
        return String::new();
    }
    let Ok(Some((body_val, body_obj))) = doc.get(&obj, BODY_KEY) else {
        return String::new();
    };
    if !matches!(body_val, Value::Object(ObjType::Text)) {
        return String::new();
    }
    doc.text(&body_obj).unwrap_or_default()
}

fn note_match_score(
    needle_lower: &str,
    title_lower: &str,
    body_lower: &str,
    body_raw: &str,
) -> Option<(i32, Option<String>)> {
    if needle_lower.is_empty() {
        return Some((0, None));
    }
    if title_lower.starts_with(needle_lower) {
        return Some((110, snippet_around(body_raw, body_lower, needle_lower)));
    }
    if title_lower.contains(needle_lower) {
        return Some((60, snippet_around(body_raw, body_lower, needle_lower)));
    }
    if body_lower.contains(needle_lower) {
        return Some((40, snippet_around(body_raw, body_lower, needle_lower)));
    }
    None
}

/// Extract ~60 chars of context around the first `needle` match in `body`.
/// Returns the raw slice, prefixed/suffixed with `…` when the snippet is
/// windowed.
fn snippet_around(body_raw: &str, body_lower: &str, needle: &str) -> Option<String> {
    if body_raw.is_empty() {
        return None;
    }
    let hit = body_lower.find(needle)?;
    let radius = 30usize;
    let start = ceil_char_boundary(body_raw, hit.saturating_sub(radius));
    let end = ceil_char_boundary(body_raw, (hit + needle.len() + radius).min(body_raw.len()));
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(&body_raw[start..end]);
    if end < body_raw.len() {
        s.push('…');
    }
    Some(s.replace('\n', " "))
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn focus_note_tab(
    chrome: &Chrome,
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    tab: TabId,
) {
    let mut st = chrome.borrow_mut();
    {
        let mut tree = st.tree_mut();
        tree.active = workspace;
        tree.active_strip.insert(workspace, strip);
        tree.active_group.insert(strip, group);
        if let Some(w) = tree.workspaces.iter_mut().find(|w| w.id == workspace) {
            if let Some(s) = w.strips.iter_mut().find(|s| s.id == strip) {
                if let Some(g) = s.window_groups.iter_mut().find(|g| g.id == group) {
                    g.active_tab = tab;
                }
            }
        }
    }
    st.bump();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::notes_app::BODY_KEY;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use automerge::AutoCommit;
    use automerge::transaction::Transactable;
    use parking_lot::RwLock;
    use std::collections::HashMap as StdMap;
    use std::sync::Arc;

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

    fn seed_note_body(doc: &mut AutoCommit, tab_id: u64, body: &str) {
        let key = tab_id.to_string();
        let tab_root = doc
            .put_object(automerge::ROOT, &key, automerge::ObjType::Map)
            .unwrap();
        let body_obj = doc
            .put_object(&tab_root, BODY_KEY, automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(&body_obj, 0, 0, body).unwrap();
    }

    #[test]
    fn matches_title_over_body_by_score() {
        let handle = tree_with_note_tab(1, "Grocery list");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let mut doc = AutoCommit::new();
        seed_note_body(&mut doc, 1, "reminder: nothing about grocery in body");
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
        let mut doc = AutoCommit::new();
        seed_note_body(&mut doc, 1, "The quick brown fox jumps.");
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

    #[test]
    fn no_hit_when_body_and_title_both_miss() {
        let handle = tree_with_note_tab(1, "Untitled");
        let chrome = ChromeState::new_with_handle(handle);
        let chrome_st = chrome.borrow();
        let mut doc = AutoCommit::new();
        seed_note_body(&mut doc, 1, "The quick brown fox jumps.");
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
