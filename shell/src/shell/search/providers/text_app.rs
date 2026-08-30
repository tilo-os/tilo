//! The half of a search provider that every CRDT-backed text app in the
//! shell shares: walk the tabs hosting that app, match the query against
//! each tab's title *and* document body, and hand back results that focus
//! the tab. Notes and Editor render differently but store content the same
//! way — one Automerge `Text` object under a well-known key (see
//! [`crate::shell::crdt_text`]) — so *where the body is read from* lives
//! here once.

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::{ResourceKind, SearchContext, SearchResult};
use crate::shell::state::{AppKind, GroupId, StripId, TabId, TileContent, WorkspaceId};
use crate::shell::tab_documents::tab_body;

/// Top of the shared score scale — a title prefix. See [`match_score`].
pub const MAX_SCORE: i32 = 110;

/// Does `tile` host the shell text app called `app`, matched case-insensitively?
pub fn tile_hosts_app(tile: &TileContent, app: &str) -> bool {
    match tile {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            matches!(kind, AppKind::Other(name) if name.eq_ignore_ascii_case(app))
        }
        _ => false,
    }
}

/// [`crate::shell::search::SearchProvider::owns_tab`] for a text app.
pub fn owns_tab_of(chrome: &ChromeState, tab_id: u64, app: &str) -> bool {
    let tree = chrome.tree();
    let found = 'search: {
        for w in &tree.workspaces {
            for s in &w.strips {
                for g in &s.window_groups {
                    for t in &g.tabs {
                        if t.id == tab_id {
                            break 'search tile_hosts_app(&t.tile, app);
                        }
                    }
                }
            }
        }
        false
    };
    drop(tree);
    found
}

/// [`crate::shell::search::SearchProvider::query`] for a text app.
/// `body_key` must match the Automerge key
/// [`CrdtTextBody`](crate::shell::crdt_text::CrdtTextBody) writes under,
/// read **per tab** out of that tab's own document.
pub fn query_text_app(
    ctx: &SearchContext<'_>,
    app: &str,
    body_key: &str,
    provider_id: &'static str,
    kind: &ResourceKind,
) -> Vec<SearchResult> {
    if !ctx.wanted.contains(kind) {
        return Vec::new();
    }
    let needle_lower = ctx.needle.to_lowercase();
    let mut out = Vec::new();
    let tree = ctx.chrome.tree();
    for w in &tree.workspaces {
        for s in &w.strips {
            for g in &s.window_groups {
                for t in &g.tabs {
                    if !tile_hosts_app(&t.tile, app) {
                        continue;
                    }
                    let title_lower = t.title.to_lowercase();
                    let body = tab_body(ctx.chrome.records(), t.id, body_key).unwrap_or_default();
                    let body_lower = body.to_lowercase();
                    let Some((score, snippet)) =
                        match_score(&needle_lower, &title_lower, &body_lower, &body)
                    else {
                        continue;
                    };
                    let (workspace_id, strip_id, group_id, tab_id) = (w.id, s.id, g.id, t.id);
                    out.push(SearchResult::nullary(
                        kind.clone(),
                        provider_id,
                        format!("{tab_id}"),
                        t.title.clone(),
                        snippet,
                        score,
                        move |_doc, _root, chrome: &Chrome| {
                            focus_tab(chrome, workspace_id, strip_id, group_id, tab_id);
                        },
                    ));
                }
            }
        }
    }
    drop(tree);
    out
}

/// Title first, then body; a body hit carries a snippet showing why it matched.
pub fn match_score(
    needle_lower: &str,
    title_lower: &str,
    body_lower: &str,
    body_raw: &str,
) -> Option<(i32, Option<String>)> {
    if needle_lower.is_empty() {
        return Some((0, None));
    }
    if title_lower.starts_with(needle_lower) {
        return Some((
            MAX_SCORE,
            snippet_around(body_raw, body_lower, needle_lower),
        ));
    }
    if title_lower.contains(needle_lower) {
        return Some((60, snippet_around(body_raw, body_lower, needle_lower)));
    }
    if body_lower.contains(needle_lower) {
        return Some((40, snippet_around(body_raw, body_lower, needle_lower)));
    }
    None
}

/// Extract ~60 chars of context around the first `needle` match, `…`-marked
/// when windowed. Line structure is flattened to single spaces — a
/// paragraph break otherwise left a visible gap mid-sentence in the row.
fn snippet_around(body_raw: &str, body_lower: &str, needle: &str) -> Option<String> {
    if body_raw.is_empty() {
        return None;
    }
    let hit = body_lower.find(needle)?;
    let radius = 30usize;
    let start = ceil_char_boundary(body_raw, hit.saturating_sub(radius));
    let end = ceil_char_boundary(
        body_raw,
        hit.saturating_add(needle.len())
            .saturating_add(radius)
            .min(body_raw.len()),
    );
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(
        &body_raw
            .get(start..end)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    );
    if end < body_raw.len() {
        s.push('…');
    }
    Some(s)
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i = i.saturating_add(1);
    }
    i
}

/// Enter on a result: switch workspace/strip/group and activate the tab.
fn focus_tab(chrome: &Chrome, workspace: WorkspaceId, strip: StripId, group: GroupId, tab: TabId) {
    chrome.borrow_mut().focus_tab(workspace, strip, group, tab);
}
