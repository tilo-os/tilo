//! The half of a search provider that every CRDT-backed text app in the
//! shell shares: walk the tabs hosting that app, match the query against
//! each tab's title *and* document body, and hand back results that focus
//! the tab. Notes and Editor render differently but store content the same
//! way — one Automerge `Text` object under a well-known key (see
//! [`crate::shell::crdt_text`]) — so *where the body is read from* lives
//! here once.

use crate::shell::chrome_state::{Chrome, ChromeState};
use std::cell::RefCell;

use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchResult,
};
use crate::shell::state::{AppKind, GroupId, StripId, TabId, TileContent, WorkspaceId};
use crate::shell::tab_documents::tab_body;

/// Top of the shared score scale — a title prefix. See [`match_score`].
pub const MAX_SCORE: i32 = 110;

/// Retained projection for the two CRDT-backed tab providers.  It is rebuilt
/// only after `ChromeState` advances, never for an ordinary query edit.
pub struct TextSearchCache {
    snapshot: RefCell<Option<TextSnapshot>>,
}

impl Default for TextSearchCache {
    fn default() -> Self {
        Self {
            snapshot: RefCell::new(None),
        }
    }
}

#[derive(Clone)]
struct TextEntry {
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    tab: TabId,
    title: String,
    title_lower: String,
    body: String,
    body_lower: String,
}

struct TextSnapshot {
    revision: String,
    entries: Vec<TextEntry>,
    index: ui::search::SearchTextIndex,
}

/// Tree membership and record content jointly define a text-app search
/// projection. Chrome revision alone misses edits made directly in the
/// record CRDT between shell-layout changes.
pub fn text_search_revision(chrome: &ChromeState) -> String {
    format!(
        "{}:{}",
        chrome.revision,
        chrome.records().content_revision()
    )
}

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
        for w in &tree.desktop.workspaces {
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

/// Resolve a text result's opaque tab id back to actual workspace
/// membership for contextual ranking.
pub fn proximity_of(
    chrome: &ChromeState,
    result: &SearchResult,
) -> crate::shell::search::SearchProximity {
    let Ok(tab_id) = result.id.parse::<u64>() else {
        return crate::shell::search::SearchProximity::Global;
    };
    let tree = chrome.tree();
    let is_here = tree.desktop.active_workspace().is_some_and(|workspace| {
        workspace.strips.iter().any(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.tabs.iter().any(|tab| tab.id == tab_id))
        })
    });
    if is_here {
        crate::shell::search::SearchProximity::CurrentWorkspace
    } else {
        crate::shell::search::SearchProximity::Global
    }
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
    // Kept for direct callers and old provider tests. The live providers use
    // `query_text_app_page` and therefore their retained snapshot.
    if !ctx.wanted.contains(kind) {
        return Vec::new();
    }
    let needle_lower = ctx.needle.to_lowercase();
    let mut out = Vec::new();
    let tree = ctx.chrome.tree();
    for w in &tree.desktop.workspaces {
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

/// Query the retained tab/body projection.  Cursors carry only a snapshot
/// offset and the Chrome revision; a changed tree invalidates rather than
/// skipping a concurrent insert or edit.
#[allow(
    clippy::too_many_arguments,
    reason = "the shared helper preserves one explicit provider contract for each text-app source without an allocation-only parameter object"
)]
pub fn query_text_app_page(
    cache: &TextSearchCache,
    ctx: &SearchContext<'_>,
    app: &str,
    body_key: &str,
    provider_id: &'static str,
    kind: &ResourceKind,
    cursor: Option<&SearchCursor>,
    budget: SearchBudget,
) -> SearchPage {
    if !ctx.wanted.contains(kind) || budget.max_results == 0 || budget.max_work == 0 {
        return SearchPage::exhausted(text_search_revision(ctx.chrome), Vec::new());
    }
    let revision = text_search_revision(ctx.chrome);
    let after = match cursor {
        Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
        Some(cursor) => {
            let Some((offset, cursor_query)) = cursor
                .token
                .strip_prefix("text:")
                .and_then(|token| token.split_once('\u{1f}'))
            else {
                return SearchPage::stale(revision);
            };
            if cursor_query != ctx.needle {
                return SearchPage::stale(revision);
            }
            let Ok(offset) = offset.parse::<usize>() else {
                return SearchPage::stale(revision);
            };
            Some(offset)
        }
        None => None,
    };
    let mut slot = cache.snapshot.borrow_mut();
    if slot
        .as_ref()
        .is_none_or(|snapshot| snapshot.revision != revision)
    {
        *slot = Some(build_snapshot(ctx, app, body_key, revision.clone()));
    }
    let Some(snapshot) = slot.as_ref() else {
        return SearchPage::exhausted(revision, Vec::new());
    };
    let page = snapshot
        .index
        .matching_page(ctx.needle, after, budget.max_work, budget.max_results);
    let needle_lower = ctx.needle.to_lowercase();
    let mut used = 0usize;
    let mut results = Vec::new();
    for id in page.ids {
        let Some(entry) = snapshot.entries.get(id) else {
            continue;
        };
        let Some((score, snippet)) = match_score(
            &needle_lower,
            &entry.title_lower,
            &entry.body_lower,
            &entry.body,
        ) else {
            continue;
        };
        let bytes = entry
            .title
            .len()
            .saturating_add(snippet.as_ref().map_or(0, String::len))
            .saturating_add(96);
        if used.saturating_add(bytes) > budget.max_bytes {
            break;
        }
        used = used.saturating_add(bytes);
        let (workspace, strip, group, tab) = (entry.workspace, entry.strip, entry.group, entry.tab);
        results.push(SearchResult::nullary(
            kind.clone(),
            provider_id,
            ui::record::RecordId::for_tab(tab).to_string(),
            entry.title.clone(),
            snippet,
            score,
            move |_doc, _root, chrome: &Chrome| focus_tab(chrome, workspace, strip, group, tab),
        ));
    }
    let exhausted = page.next_after.is_none();
    SearchPage {
        revision: revision.clone(),
        results,
        next_cursor: page.next_after.map(|offset| SearchCursor {
            revision,
            token: format!("text:{offset}\u{1f}{}", ctx.needle),
        }),
        exhausted,
        stale_cursor: false,
        budget_exhausted: !exhausted,
    }
}

fn build_snapshot(
    ctx: &SearchContext<'_>,
    app: &str,
    body_key: &str,
    revision: String,
) -> TextSnapshot {
    let mut entries = Vec::new();
    let tree = ctx.chrome.tree();
    for workspace in &tree.desktop.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if !tile_hosts_app(&tab.tile, app) {
                        continue;
                    }
                    let body = tab_body(ctx.chrome.records(), tab.id, body_key).unwrap_or_default();
                    entries.push(TextEntry {
                        workspace: workspace.id,
                        strip: strip.id,
                        group: group.id,
                        tab: tab.id,
                        title_lower: tab.title.to_lowercase(),
                        title: tab.title.clone(),
                        body_lower: body.to_lowercase(),
                        body,
                    });
                }
            }
        }
    }
    drop(tree);
    let index = ui::search::SearchTextIndex::new(
        entries
            .iter()
            .map(|entry| format!("{}\u{1f}{}", entry.title_lower, entry.body_lower)),
    );
    TextSnapshot {
        revision,
        entries,
        index,
    }
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
