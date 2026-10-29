//! How a window's resource answers search: its entries and their index, the
//! provider the system's search reaches it through, and the pages a search
//! that waits on its surfaces' answers continues in.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use tilo_resource::{ScopeDecl, SearchEntry};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use ui_core::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, PagePoll, PendingToken,
    SearchCursor, SearchPage, SearchProvider, SearchQuery, SearchResult,
};

use super::SearchCache;
use crate::framework::publishing::SearchQueries;

pub(crate) struct BodyText(pub(crate) Arc<dyn tilo_resource::SearchText>);

impl tilo_search::BodySource for BodyText {
    fn segment_count(&self) -> usize {
        self.0.segment_count()
    }

    fn piece_count(&self, segment: usize) -> usize {
        self.0.piece_count(segment)
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<tilo_search::BodyPiece<'_>> {
        self.0
            .piece(segment, piece)
            .map(|piece| tilo_search::BodyPiece {
                text: piece.text,
                space_before: piece.space_before,
            })
    }

    fn locates(&self, segment: usize) -> bool {
        self.0.locates(segment)
    }
}

/// A resource's search entries and their index, keyed by position.
pub(crate) struct IndexedEntries {
    entries: Vec<SearchEntry>,
    facets: Vec<ui_core::search::Facet>,
    pub(crate) index: SearchIndex,
    /// Which build of the entries this is: a page cursor from another build
    /// starts over rather than skipping or repeating.
    pub(crate) generation: u64,
}

impl IndexedEntries {
    pub(crate) fn built(entries: Vec<SearchEntry>, generation: u64) -> Self {
        // Entry titles are names a person half remembers: typos match too.
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(entries.iter().enumerate().map(|(at, entry)| {
            let mut document = Document::new(at as u64)
                .field(entry.title.as_str(), FieldWeight::TITLE)
                .field(entry.text.as_str(), FieldWeight::BODY)
                .field(
                    entry.subtitle.as_deref().unwrap_or_default(),
                    FieldWeight::SUBTITLE,
                );
            for (text, weight) in &entry.fields {
                document = document.field(text.as_str(), field_weight(*weight));
            }
            match &entry.body {
                Some(body) => document.body(Arc::new(BodyText(body.shared()))),
                None => document,
            }
        }));
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index resource search entries");
        }
        let mut groups: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
        for entry in &entries {
            for (key, label, value) in &entry.facets {
                groups
                    .entry(key.clone())
                    .or_insert_with(|| (label.clone(), BTreeSet::new()))
                    .1
                    .insert(value.clone());
            }
        }
        let facets = groups
            .into_iter()
            .map(|(key, (label, values))| {
                values
                    .into_iter()
                    .fold(ui_core::search::Facet::new(key, label), |facet, value| {
                        facet.value(value.clone(), value)
                    })
            })
            .collect();
        Self {
            entries,
            facets,
            index,
            generation,
        }
    }
}

/// `entry` as the session's persistent search index keeps it: keyed by the
/// resource a hit opens, with its short fields. A prose body stays in the
/// app, and a secret-bearing entry carries only its title, secondary line
/// and fields (what the app chose to show of it, never a secret value).
/// `None` for an entry a hit cannot open from outside the app.
pub(crate) fn index_entry(
    entry: &SearchEntry,
) -> Option<ui_core::search::index_publication::IndexEntry> {
    use tilo_resource::SearchWeight;
    use ui_core::search::index_publication::{IndexAction, IndexEntry, IndexWeight};
    let action = entry.action.as_ref().map(|address| IndexAction {
        resource: address.resource.clone(),
        action: address.action.clone(),
    });
    let resource = entry
        .anchor
        .clone()
        .or_else(|| action.as_ref().map(|action| action.resource.clone()))?;
    let key = match &action {
        Some(action) => format!("{}\u{1f}{}", action.resource, action.action),
        None => resource.clone(),
    };
    let mut fields = Vec::new();
    if !entry.secret && !entry.text.trim().is_empty() {
        fields.push((entry.text.clone(), IndexWeight::Body));
    }
    fields.extend(entry.fields.iter().filter_map(|(text, weight)| {
        let weight = match weight {
            SearchWeight::Title => IndexWeight::Title,
            SearchWeight::Identity => IndexWeight::Identity,
            SearchWeight::Subtitle => IndexWeight::Subtitle,
            SearchWeight::Metadata => IndexWeight::Metadata,
            SearchWeight::Body => IndexWeight::Body,
            SearchWeight::Unranked => return None,
        };
        Some((text.clone(), weight))
    }));
    let subtitle = entry
        .subtitle
        .clone()
        .or_else(|| entry.row.as_ref().and_then(|row| row.subtitle.clone()));
    let icon = entry
        .icon
        .clone()
        .or_else(|| entry.row.as_ref().and_then(|row| row.icon.clone()));
    Some(IndexEntry {
        key,
        resource,
        action,
        title: entry.title.clone(),
        subtitle,
        group: entry.group.clone(),
        icon,
        boost: entry.boost,
        fields,
        secret: entry.secret,
        references: Vec::new(),
    })
}

/// The index's weight for a field an entry ranks as `weight`.
pub(crate) fn field_weight(weight: tilo_resource::SearchWeight) -> FieldWeight {
    use tilo_resource::SearchWeight;
    match weight {
        SearchWeight::Title => FieldWeight::TITLE,
        SearchWeight::Identity => FieldWeight::IDENTITY,
        SearchWeight::Subtitle => FieldWeight::SUBTITLE,
        SearchWeight::Metadata => FieldWeight::METADATA,
        SearchWeight::Body => FieldWeight::BODY,
        SearchWeight::Unranked => FieldWeight::UNRANKED,
    }
}

/// A search provider over one snapshot of the resource's entries.
pub(crate) struct ResourceSearch {
    pub(crate) name: &'static str,
    pub(crate) indexed: Arc<IndexedEntries>,
    pub(crate) scopes: Vec<ScopeDecl>,
    pub(crate) queries: Vec<SearchQueries>,
    /// Where a search that waits on the surfaces' answers finds them: none
    /// for a fixed set of entries.
    pub(crate) live: Option<LiveSearch>,
    /// The resource's `open`, where a view shows its items: how a host
    /// searching from outside the app opens a hit at its anchor.
    pub(crate) opens: Option<tilo_actions::ActionAddress>,
}

/// The resource's index as it moves on, for a search that waits on its
/// surfaces' answers.
pub(crate) struct LiveSearch {
    pub(crate) cache: Rc<std::cell::RefCell<SearchCache>>,
    /// The index holding the answers, once they are in: later pages
    /// continue in it.
    pub(crate) answered: std::cell::RefCell<Option<Arc<IndexedEntries>>>,
}

/// `result` read as the entry's row: its subtitle, its icon and its shape.
fn with_row(mut result: SearchResult, title: &str, row: &tilo_resource::SearchRow) -> SearchResult {
    use tilo_resource::SearchRowShape;
    use ui_core::search::SearchRow;
    if let Some(subtitle) = &row.subtitle {
        result = result.with_subtitle(subtitle.clone());
    }
    if let Some(icon) = row
        .icon
        .as_deref()
        .and_then(ui_core::icons::Icon::from_name)
    {
        result = result.with_icon(icon);
    }
    let shaped = match &row.shape {
        SearchRowShape::Plain => None,
        SearchRowShape::Message {
            sender,
            snippet,
            date,
            unread,
        } => {
            let mut shaped = SearchRow::message(sender.clone(), title).unread(*unread);
            if let Some(snippet) = snippet {
                shaped = shaped.snippet(snippet.clone());
            }
            if let Some(date) = date {
                shaped = shaped.date(date.clone());
            }
            Some(shaped)
        }
        SearchRowShape::Media { byline, duration } => {
            let mut shaped = SearchRow::media(title);
            if let Some(byline) = byline {
                shaped = shaped.byline(byline.clone());
            }
            if let Some(duration) = duration {
                shaped = shaped.duration(duration.clone());
            }
            Some(shaped)
        }
        SearchRowShape::FileHit {
            path,
            line,
            snippet,
        } => {
            let mut shaped = SearchRow::file_hit(path.clone(), snippet.clone());
            if let Some(line) = line {
                shaped = shaped.line(*line);
            }
            Some(shaped)
        }
    };
    match shaped {
        Some(shaped) => result.with_row(shaped),
        None => result,
    }
}

impl ResourceSearch {
    /// The search source over `indexed`, listed under `name` unless an
    /// entry names its own group.
    pub(crate) fn new(name: &'static str, indexed: Arc<IndexedEntries>) -> Self {
        Self {
            name,
            indexed,
            scopes: Vec::new(),
            queries: Vec::new(),
            live: None,
            opens: None,
        }
    }

    /// A token for the answers still coming from the surfaces, if any are:
    /// the search showing keeps asking for them.
    fn waiting(&self) -> Option<PendingToken> {
        let live = self.live.as_ref()?;
        if !self.queries.iter().any(SearchQueries::answering) {
            return None;
        }
        let mut cache = live.cache.borrow_mut();
        cache.watching = true;
        cache.answered = None;
        Some(PendingToken::new(self.indexed.generation))
    }

    /// The index this search answers from: the surfaces' answers once they
    /// are in.
    fn indexed(&self) -> Arc<IndexedEntries> {
        self.live
            .as_ref()
            .and_then(|live| live.answered.borrow().clone())
            .unwrap_or_else(|| Arc::clone(&self.indexed))
    }
}

impl SearchProvider for ResourceSearch {
    fn name(&self) -> &str {
        self.name
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    // No caller materialises more than one interactive page in one turn;
    // complete discovery goes through `search_page`'s cursor.
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let typed = SearchQuery::new(query);
        for queries in &self.queries {
            queries.submit(&typed);
        }
        self.results(&typed, Page::first(limit.min(DEFAULT_SEARCH_RESULT_LIMIT)))
            .0
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.indexed().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        cursor.opaque_parts().0 == self.indexed().generation
    }

    // Every entry stays findable however many there are: a page continues
    // where the last one stopped, within the caller's budgets.
    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        for queries in &self.queries {
            queries.submit(query);
        }
        let pending = self.waiting();
        let limit = work_budget
            .min(DEFAULT_SEARCH_CANDIDATE_LIMIT)
            .min(result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT));
        let folded = format!(
            "{}\0{:?}",
            ui_core::search::normalize_search_text(query.text()),
            query.filters()
        );
        let indexed = self.indexed();
        let offset = cursor
            .map(SearchCursor::opaque_parts)
            .filter(|(generation, at_query, at_scope, _)| {
                *generation == indexed.generation
                    && *at_query == folded
                    && *at_scope == query.scope()
            })
            .and_then(|(_, _, _, offset)| usize::try_from(offset?).ok())
            .unwrap_or(0);
        if limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
                pending,
            };
        }
        let (results, more, inspected) = self.results(query, Page { offset, limit });
        let next = more.then(|| {
            SearchCursor::opaque(
                indexed.generation,
                folded,
                query.scope().map(str::to_owned),
                u64::try_from(offset.saturating_add(inspected)).ok(),
            )
        });
        SearchPage {
            results,
            next,
            inspected,
            pending,
        }
    }

    // What the surfaces answer later (a server's results) reaches the
    // search showing them once all of them have answered: the latest query
    // asked again of the index holding the answers.
    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        let Some(live) = &self.live else {
            return PagePoll::Ready(SearchPage::empty());
        };
        if self.queries.iter().any(SearchQueries::answering) {
            return PagePoll::Pending;
        }
        let Some(answer) = live.cache.borrow_mut().answer() else {
            return PagePoll::Pending;
        };
        let _ = token;
        *live.answered.borrow_mut() = Some(answer);
        let Some(query) = self.queries.iter().find_map(SearchQueries::last) else {
            return PagePoll::Ready(SearchPage::empty());
        };
        PagePoll::Answered(self.search_page(
            &query,
            None,
            DEFAULT_SEARCH_CANDIDATE_LIMIT,
            DEFAULT_SEARCH_RESULT_LIMIT,
        ))
    }

    fn facets(&self) -> Vec<ui_core::search::Facet> {
        self.indexed().facets.clone()
    }

    fn search_scopes(&self) -> Vec<ScopeDecl> {
        self.scopes.clone()
    }

    fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
        self.opens.clone()
    }
}

impl ResourceSearch {
    /// One page of hits for `query`, and whether more follow it.
    fn results(&self, query: &SearchQuery, page: Page) -> (Vec<SearchResult>, bool, usize) {
        let indexed = self.indexed();
        let found = indexed.index.search(query.text(), page);
        let inspected = found.hits.len();
        let results = found
            .hits
            .into_iter()
            .filter_map(|hit| {
                let parent = indexed.entries.get(usize::try_from(hit.key).ok()?)?;
                // A match in a located segment of a body is that location's
                // hit, listed with its entry unless it names its own group.
                let located = hit.segment.and_then(|segment| {
                    let mut location = parent.body.as_ref()?.text().location(segment)?;
                    if location.group.is_none() {
                        location.group.clone_from(&parent.group);
                    }
                    Some(location)
                });
                let entry = located.as_ref().unwrap_or(parent);
                if query.scope().is_some_and(|scope| !entry.in_scope(scope))
                    || query.filters().iter().any(|filter| {
                        !parent.facets.iter().any(|(key, _, value)| {
                            key.eq_ignore_ascii_case(&filter.key)
                                && query.filter_values(&filter.key).any(|selected| {
                                    ui_core::search::normalize_search_text(value)
                                        == ui_core::search::normalize_search_text(selected)
                                })
                        })
                    })
                {
                    return None;
                }
                let group = entry.group.as_deref().unwrap_or(self.name);
                let mut result = match &entry.action {
                    Some(action) => SearchResult::for_action(group, entry.title.clone(), action),
                    None => SearchResult::new(
                        group,
                        entry.title.clone(),
                        entry.anchor.clone().unwrap_or_default(),
                    ),
                };
                if let Some(subtitle) = &entry.subtitle {
                    result = result.with_subtitle(subtitle.clone());
                }
                if let Some(icon) = entry
                    .icon
                    .as_deref()
                    .and_then(ui_core::icons::Icon::from_name)
                {
                    result = result.with_icon(icon);
                }
                result = match &entry.preview {
                    Some(preview) => result.with_preview(ui_core::search::SearchPreview {
                        title: preview.title.clone(),
                        facts: preview.facts.clone(),
                        lines: preview.lines.clone(),
                        body: preview.body.clone(),
                    }),
                    None => result.with_preview(entry.text.clone()),
                };
                if let Some(within) = &entry.within {
                    result = result.within(within.clone());
                }
                if let Some(row) = &entry.row {
                    result = with_row(result, &entry.title, row);
                }
                Some(result.with_score(hit.rank.unwrap_or(0).saturating_add(entry.boost)))
            })
            .collect();
        (results, found.more, inspected)
    }
}
