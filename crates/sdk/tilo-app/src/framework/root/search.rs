//! How a window's resource answers search: its entries and their index, the
//! provider the system's search reaches it through, and the pages a search
//! that waits on its surfaces' answers continues in.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

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

/// A resource's search entries and their index.
///
/// Every build after the first is a change to the one before it: an entry
/// the resource still lists unchanged keeps its place in the index, and only
/// the entries that came, went or changed are indexed. A notebook of a
/// thousand long notes or a tracker of 25,000 issues costs, per edit, the
/// edited note or issue rather than all of them. A change to most of the
/// entries is indexed afresh, as the first build is.
///
/// The index is shared by every build that changed it: a search holding an
/// earlier build finds what the latest one holds.
pub(crate) struct IndexedEntries {
    content: Arc<Mutex<Indexed>>,
    /// Which build of the entries this is: a page cursor from another build
    /// starts over rather than skipping or repeating.
    pub(crate) generation: u64,
}

/// The entries of the latest build, by the key the index holds each under.
struct Indexed {
    entries: HashMap<u64, SearchEntry>,
    /// The keys of the entries with each [`fingerprint`], for finding what
    /// a later build still lists.
    keys_by_print: HashMap<u64, Vec<u64>>,
    /// The key the next new entry is indexed under. Keys rise in the order
    /// entries arrive, so hits of equal rank keep the order the resource
    /// lists its entries in, newer entries after older ones.
    next_key: u64,
    /// How many entries offer each value of each facet: the facet's label,
    /// and its values' counts, by its key.
    facets: Facets,
    index: SearchIndex,
    /// How many entries were indexed, over every build.
    #[cfg(test)]
    indexings: usize,
}

/// More changed than this share of a build's entries is indexed afresh: the
/// index's own bulk build is then cheaper than the changes one by one, and
/// is built aside rather than in the index searches read.
const FRESH_SHARE: usize = 4;

impl IndexedEntries {
    pub(crate) fn built(entries: Vec<SearchEntry>, generation: u64) -> Self {
        Self {
            content: Arc::new(Mutex::new(Indexed::fresh(entries))),
            generation,
        }
    }

    /// `entries` indexed as a change to this build: only what came, went or
    /// changed since it is indexed.
    pub(crate) fn updated(&self, entries: Vec<SearchEntry>, generation: u64) -> Self {
        let mut content = self.content.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(changes) = content.changes(&entries) else {
            drop(content);
            return Self::built(entries, generation);
        };
        content.apply(changes, entries);
        drop(content);
        Self {
            content: Arc::clone(&self.content),
            generation,
        }
    }

    fn content(&self) -> std::sync::MutexGuard<'_, Indexed> {
        self.content.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The facets the entries offer.
    pub(crate) fn facets(&self) -> Vec<ui_core::search::Facet> {
        self.content()
            .facets
            .iter()
            .map(|(key, (label, values))| {
                values.keys().fold(
                    ui_core::search::Facet::new(key.clone(), label.clone()),
                    |facet, value| facet.value(value.clone(), value.clone()),
                )
            })
            .collect()
    }

    /// How many entries were indexed to bring this index where it is.
    #[cfg(test)]
    pub(crate) fn indexed_count(&self) -> usize {
        self.content().indexings
    }
}

/// What a build changes of the one before it: the keys of the entries it no
/// longer lists, and the positions of its entries the earlier one does not
/// hold.
struct Changes {
    gone: Vec<u64>,
    added: Vec<(usize, u64)>,
}

impl Indexed {
    fn fresh(entries: Vec<SearchEntry>) -> Self {
        // Entry titles are names a person half remembers: typos match too.
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(
            entries
                .iter()
                .enumerate()
                .map(|(at, entry)| document(at as u64, entry)),
        );
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index resource search entries");
        }
        let mut keys_by_print: HashMap<u64, Vec<u64>> = HashMap::new();
        for (at, entry) in entries.iter().enumerate() {
            keys_by_print
                .entry(fingerprint(entry))
                .or_default()
                .push(at as u64);
        }
        let mut facets = Facets::new();
        for entry in &entries {
            count_facets(&mut facets, entry, true);
        }
        Self {
            next_key: entries.len() as u64,
            #[cfg(test)]
            indexings: entries.len(),
            entries: entries
                .into_iter()
                .enumerate()
                .map(|(at, entry)| (at as u64, entry))
                .collect(),
            keys_by_print,
            facets,
            index,
        }
    }

    /// What `entries` change of what is indexed, or `None` when they change
    /// so much that indexing them afresh is cheaper.
    fn changes(&self, entries: &[SearchEntry]) -> Option<Changes> {
        let mut kept: HashSet<u64> = HashSet::with_capacity(entries.len());
        let mut added = Vec::new();
        let limit = entries.len().max(self.entries.len()) / FRESH_SHARE;
        for (at, entry) in entries.iter().enumerate() {
            let print = fingerprint(entry);
            let same = self.keys_by_print.get(&print).and_then(|keys| {
                keys.iter().copied().find(|key| {
                    !kept.contains(key) && self.entries.get(key).is_some_and(|held| held == entry)
                })
            });
            match same {
                Some(key) => {
                    kept.insert(key);
                }
                None => added.push((at, print)),
            }
            if added.len() > limit {
                return None;
            }
        }
        let gone: Vec<u64> = self
            .entries
            .keys()
            .copied()
            .filter(|key| !kept.contains(key))
            .collect();
        (added.len().saturating_add(gone.len()) <= limit).then_some(Changes { gone, added })
    }

    /// Index `changes`, which `entries` make.
    fn apply(&mut self, changes: Changes, entries: Vec<SearchEntry>) {
        if changes.gone.is_empty() && changes.added.is_empty() {
            return;
        }
        for key in &changes.gone {
            self.index.remove(*key);
            if let Some(entry) = self.entries.remove(key) {
                count_facets(&mut self.facets, &entry, false);
                let print = fingerprint(&entry);
                if let Some(keys) = self.keys_by_print.get_mut(&print) {
                    keys.retain(|held| held != key);
                    if keys.is_empty() {
                        self.keys_by_print.remove(&print);
                    }
                }
            }
        }
        let mut entries: Vec<Option<SearchEntry>> = entries.into_iter().map(Some).collect();
        for (at, print) in changes.added {
            let Some(entry) = entries.get_mut(at).and_then(Option::take) else {
                continue;
            };
            let key = self.next_key;
            self.next_key = key.saturating_add(1);
            self.index.upsert(document(key, &entry));
            self.keys_by_print.entry(print).or_default().push(key);
            count_facets(&mut self.facets, &entry, true);
            self.entries.insert(key, entry);
            #[cfg(test)]
            {
                self.indexings = self.indexings.saturating_add(1);
            }
        }
        if let Err(error) = self.index.commit() {
            tracing::warn!(%error, "could not index resource search entries");
        }
    }
}

/// `entry` as the index keeps it, under `key`.
fn document(key: u64, entry: &SearchEntry) -> Document {
    let mut document = Document::new(key)
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
}

/// A hash of what tells `entry` apart from the entries beside it, for
/// finding it among an earlier build's: two equal entries have the same
/// one. A body is told apart by the text it shares, as entries compare it.
fn fingerprint(entry: &SearchEntry) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    entry.title.hash(&mut hasher);
    entry.text.hash(&mut hasher);
    entry.anchor.hash(&mut hasher);
    entry.subtitle.hash(&mut hasher);
    entry.group.hash(&mut hasher);
    entry
        .action
        .as_ref()
        .map(|action| (&action.resource, &action.action))
        .hash(&mut hasher);
    entry
        .body
        .as_ref()
        .map(|body| Arc::as_ptr(&body.shared()).cast::<()>() as usize)
        .hash(&mut hasher);
    hasher.finish()
}

/// The label of each facet by its key, and how many entries offer each of
/// its values.
type Facets = BTreeMap<String, (String, BTreeMap<String, usize>)>;

/// Count the facet values `entry` offers in `facets`, or take them away.
fn count_facets(facets: &mut Facets, entry: &SearchEntry, add: bool) {
    for (key, label, value) in &entry.facets {
        if add {
            let (_, values) = facets
                .entry(key.clone())
                .or_insert_with(|| (label.clone(), BTreeMap::new()));
            let count = values.entry(value.clone()).or_default();
            *count = count.saturating_add(1);
            continue;
        }
        let Some((_, values)) = facets.get_mut(key) else {
            continue;
        };
        if let Some(count) = values.get_mut(value) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                values.remove(value);
            }
        }
        if values.is_empty() {
            facets.remove(key);
        }
    }
}

/// `entry` as the session's persistent search index keeps it: keyed by the
/// resource a hit opens, with its short fields. A prose body stays in the
/// app, and a secret-bearing entry carries only its title, secondary line
/// and fields (what the app chose to show of it, never a secret value).
/// An entry invokes its own action, else its anchor's `open` where the
/// window publishes one there (`is_item`), else the `open` of `opens`, the
/// resource holding the anchor, told the anchor. `None` for an entry a hit
/// cannot open from outside the app.
pub(crate) fn index_entry(
    entry: &SearchEntry,
    opens: &str,
    is_item: &dyn Fn(&str) -> bool,
) -> Option<ui_core::search::index_publication::IndexEntry> {
    use tilo_resource::SearchWeight;
    use ui_core::search::index_publication::{IndexAction, IndexEntry, IndexWeight};
    let (key, resource, action) = match (&entry.action, &entry.anchor) {
        (Some(address), anchor) => (
            format!("{}\u{1f}{}", address.resource, address.action),
            anchor.clone().unwrap_or_else(|| address.resource.clone()),
            Some(address.clone()),
        ),
        (None, Some(anchor)) => (
            anchor.clone(),
            anchor.clone(),
            (!is_item(anchor)).then(|| {
                tilo_actions::ActionAddress::new(
                    opens,
                    tilo_actions::remote::APPLICATION_OPEN_ACTION,
                )
            }),
        ),
        (None, None) => return None,
    };
    let action = action.map(|address| IndexAction {
        resource: address.resource,
        action: address.action,
    });
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
        kind: entry.kind,
        people: entry.people.clone(),
        place: entry.place.clone(),
        when: entry.when,
        boost: entry.boost,
        fields,
        secret: entry.secret,
        references: Vec::new(),
        // A picture's bytes are named by the app that keeps it, in the entry it
        // publishes for search, or in the text a picture's own record holds.
        image: entry.picture.clone().or_else(|| {
            std::iter::once(&entry.text)
                .chain(entry.fields.iter().map(|(text, _)| text))
                .find_map(|text| tilo_resource::search_index::picture_address(text))
        }),
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
    /// The app that keeps the resource, as its item hits name it.
    pub(crate) app: &'static str,
    pub(crate) indexed: Arc<IndexedEntries>,
    pub(crate) scopes: Vec<ScopeDecl>,
    pub(crate) queries: Vec<SearchQueries>,
    /// Whether the person this window is open for may read the record: one
    /// who may not is found nothing, as they are shown nothing.
    pub(crate) readable: bool,
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
    /// The search source over `indexed`, of the app `app`, listed under
    /// `name` unless an entry names its own group. Everyone who opens the
    /// app may read its record; a window that says otherwise says so.
    pub(crate) fn new(name: &'static str, app: &'static str, indexed: Arc<IndexedEntries>) -> Self {
        Self {
            name,
            app,
            indexed,
            scopes: Vec::new(),
            queries: Vec::new(),
            readable: true,
            live: None,
            opens: None,
        }
    }

    /// A token for the rows still coming, if any are: the search showing
    /// keeps asking for them. Two kinds — the surfaces' answers to a server
    /// query, and the index of this window's own content, built off the
    /// frame now, so the build's rows join the ones the search already shows
    /// when it lands.
    fn waiting(&self) -> Option<PendingToken> {
        let live = self.live.as_ref()?;
        let surfaces = self.queries.iter().any(SearchQueries::answering);
        let building = live.cache.borrow_mut().building();
        if !surfaces && !building {
            return None;
        }
        let mut cache = live.cache.borrow_mut();
        if surfaces {
            cache.watching = true;
            cache.answered = None;
        }
        Some(PendingToken::new(self.indexed().generation))
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
        // The window's own index is built off the frame: until it lands, the
        // rows shown are the last index's, and the search looks again.
        if live.cache.borrow_mut().building() {
            return PagePoll::Pending;
        }
        // A build that landed answers with the rows it found; a search only
        // waiting on a build (no surface answer) has nothing in `answer`.
        let answer = live
            .cache
            .borrow_mut()
            .answer()
            .unwrap_or_else(|| self.indexed());
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
        if !self.readable {
            return Vec::new();
        }
        self.indexed().facets()
    }

    fn search_scopes(&self) -> Vec<ScopeDecl> {
        if !self.readable {
            return Vec::new();
        }
        self.scopes.clone()
    }

    fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
        if !self.readable {
            return None;
        }
        self.opens.clone()
    }
}

impl ResourceSearch {
    /// One page of hits for `query`, and whether more follow it.
    fn results(&self, query: &SearchQuery, page: Page) -> (Vec<SearchResult>, bool, usize) {
        if !self.readable {
            return (Vec::new(), false, 0);
        }
        let indexed = self.indexed();
        let content = indexed.content();
        let found = content.index.search(query.text(), page);
        let inspected = found.hits.len();
        let results = found
            .hits
            .into_iter()
            .filter_map(|hit| {
                let parent = content.entries.get(&hit.key)?;
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
                match &entry.row {
                    Some(row) => result = with_row(result, &entry.title, row),
                    // An item that says nothing more about how it reads is
                    // the reference its app draws: the app, the item's kind
                    // and its id, opening the item.
                    None => {
                        if let Some(row) = self.item_row(entry) {
                            result = result.with_row(row);
                        }
                    }
                }
                Some(result.with_score(hit.rank.unwrap_or(0).saturating_add(entry.boost)))
            })
            .collect();
        drop(content);
        (results, found.more, inspected)
    }

    /// `entry` as the reference the app draws, for an entry that is an item
    /// of this resource and invokes no action of its own: `None` for a hit
    /// that is not one (a video from a service, a row of the window's own).
    fn item_row(&self, entry: &SearchEntry) -> Option<ui_core::search::SearchRow> {
        if entry.action.is_some() {
            return None;
        }
        let anchor = entry.anchor.as_deref()?;
        let kind = tilo_resource::items::item_kind(anchor)?;
        let id = anchor.rsplit('/').next()?;
        Some(ui_core::search::SearchRow::item(self.app, kind, id))
    }
}
