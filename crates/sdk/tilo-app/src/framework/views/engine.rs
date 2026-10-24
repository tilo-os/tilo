//! The query behind a query view: the app's local items answered from an
//! index in the frame of the keystroke, and its remote sources streamed in
//! below them.
//!
//! - **Local.** The app hands its items over whenever they change
//!   ([`QueryEngine::sync_local`]); the index takes only what changed. A query
//!   is answered from it before [`QueryEngine::set_query`] returns.
//! - **Remote.** Each [`RemoteSource`] is asked off the UI thread. A query
//!   goes out at once unless one went out less than [`THROTTLE`] ago, in
//!   which case it goes out when that much has passed, and only the newest
//!   query does; asking again cancels the request before it. Answers are
//!   cached by query: asking the same again shows them at once, and a query
//!   that goes on from a cached one ("auror" after "auro") shows what of the
//!   earlier answer still matches while its own is on the way. Focusing the
//!   field warms the sources ([`QueryEngine::warm`]).
//! - **Stable rows.** Remote rows come in their own sections, below the
//!   local ones, and an answer never touches a local row. While the person
//!   points at or has the keyboard in a remote section
//!   ([`QueryEngine::hold`]), what arrives only goes on at its end: no row
//!   they could be aiming at moves.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_resource::SearchBody;
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use ui_core::WakeHandle;
use ui_core::arriving::{Arrival, ArrivalClock, Source, Step};
use ui_core::background::Background;
use ui_core::time::{Duration, Instant};

use super::super::root::{BodyText, field_weight};
use super::kind::{FieldRole, Form, Item, KindDecl};
use super::query::QuerySpec;

/// The least time between two requests to one remote source while the
/// person types. A query typed within it goes out when it has passed.
pub const THROTTLE: Duration = Duration::from_millis(40);

/// How long a remote answer is shown from the cache without asking again.
pub const FRESH: Duration = Duration::from_secs(300);

/// How many answers each remote source keeps.
const CACHED_ANSWERS: usize = 48;

/// How long warming a source lasts before focusing the field warms it again.
const WARM_FOR: Duration = Duration::from_secs(60);

/// How many local matches are looked at for one query.
const LOCAL_CANDIDATES: usize = 600;

/// How many items one group of results shows.
pub const GROUP_LIMIT: usize = 50;

/// Asked by a running request whether it should stop: a newer query took
/// its place.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Why a remote source could not answer, as its section says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteError(pub String);

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A source of items that are not local: a streaming catalogue, a store, a
/// collaborator's library. It keeps its own index; nothing of it is
/// downloaded to be searched.
pub trait RemoteSource: Send + Sync + 'static {
    /// A stable id, for its cache and its section.
    fn id(&self) -> &str;

    /// What its section is headed ("`YouTube` Music").
    fn label(&self) -> String;

    /// Where its answers come from, as its section's placeholder names it
    /// while one is on the way: a host serving the catalogue by default; a
    /// collaborator's library is a peer.
    fn origin(&self) -> Source {
        Source::host(self.label())
    }

    /// Whether it answers `query` at all: a source does not know the
    /// person's own marks (Liked), so a query narrowed by them stays local.
    fn accepts(&self, query: &QuerySpec) -> bool {
        !query.text.trim().is_empty()
    }

    /// Open the connection before the first query: the person focused the
    /// field. Runs off the UI thread.
    fn warm(&self) {}

    /// Answer `query`, handing each page of items to `page` as it arrives,
    /// best first. Runs off the UI thread; stop early when `cancel` says
    /// so.
    ///
    /// # Errors
    ///
    /// The source cannot be reached, or refused the query.
    fn search(
        &self,
        query: &QuerySpec,
        cancel: &Cancel,
        page: &mut dyn FnMut(Vec<Item>),
    ) -> Result<(), RemoteError>;
}

/// An index of the app's own that answers with the local items, in the
/// same frame: matches in what this device does not hold yet, such as a
/// library's synced index of books whose text is elsewhere. It answers on
/// the UI thread, so within the frame's budget.
pub trait LocalSource: Send + Sync + 'static {
    /// The items `query` finds, best first, at most `limit`.
    fn search(&self, query: &QuerySpec, limit: usize) -> Vec<Item>;
}

/// The items of one type in a list of results, best first.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultGroup {
    pub kind: &'static str,
    /// The type's plural ("Songs").
    pub label: String,
    pub form: Form,
    pub items: Vec<Item>,
    /// How many more matched than the group shows.
    pub more: usize,
}

/// What one remote source has found for the query.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteSection {
    pub source: String,
    pub label: String,
    /// Where its answer stands: pending (its rows streaming in), answered,
    /// or unreachable, in words (empty when it gave none). `None` when the
    /// query is not one it answers.
    pub state: Option<Arrival<(), RemoteError>>,
    /// The timing of its answer, for its placeholder
    /// ([`ui_core::widget::ArrivalFace`]): set when it was asked.
    pub clock: Option<ArrivalClock>,
    /// Its rows, as groups in the order they are shown.
    pub groups: Vec<ResultGroup>,
}

impl RemoteSection {
    /// A section of a source not asked.
    fn unasked(source: String, label: String) -> Self {
        Self {
            source,
            label,
            state: None,
            clock: None,
            groups: Vec::new(),
        }
    }

    /// Whether its answer is still on the way.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        self.state.as_ref().is_some_and(Arrival::is_pending)
    }

    /// Whether it answered in full.
    #[must_use]
    pub fn is_answered(&self) -> bool {
        matches!(self.state, Some(Arrival::Ready(())))
    }

    /// Why it could not answer, once it could not.
    #[must_use]
    pub fn failure(&self) -> Option<&RemoteError> {
        self.state.as_ref().and_then(Arrival::failure)
    }

    /// The timing of the placeholder that stands where its rows will be:
    /// while its answer is on the way and no row has come yet. A painter
    /// paints [`ui_core::widget::ArrivalFace`] from it in the space of a row.
    #[must_use]
    pub fn placeholder(&self) -> Option<&ArrivalClock> {
        if self.is_waiting() && self.groups.is_empty() {
            self.clock.as_ref()
        } else {
            None
        }
    }

    /// It was asked at `now`; its answer comes from `from`.
    fn ask(&mut self, from: Source, now: Instant) {
        self.state = Some(Arrival::Pending { from: from.clone() });
        self.clock = Some(ArrivalClock::new(now, from));
    }

    /// Its answer (or why there is none) landed at `now`.
    fn settle(&mut self, answer: Result<(), RemoteError>, now: Instant) {
        self.state = Some(answer.into());
        if let Some(clock) = self.clock.as_mut() {
            clock.arrive(now);
        }
    }

    /// The rows shown, in order.
    pub fn rows(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|group| &group.items)
    }

    fn holds(&self, item: &Item) -> bool {
        self.rows().any(|row| row.key() == item.key())
    }
}

/// What a query has found so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResults {
    /// Which query this is: bumped by every change of the query.
    pub generation: u64,
    pub query: QuerySpec,
    /// The local matches, a group per type in the types' order.
    pub local: Vec<ResultGroup>,
    /// A section per remote source, in the order the sources were given.
    pub remote: Vec<RemoteSection>,
}

impl QueryResults {
    /// Whether a remote answer is still on the way: the window's loading
    /// line shows while it is.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.remote.iter().any(RemoteSection::is_waiting)
    }

    /// The placeholder step each section shows at `now` (`None` where it
    /// shows none): a painter that kept what it drew repaints when this
    /// changes.
    #[must_use]
    pub fn placeholder_steps(&self, now: Instant) -> Vec<Option<Step>> {
        self.remote
            .iter()
            .map(|section| section.placeholder().map(|clock| clock.step(now)))
            .collect()
    }

    /// When a section's placeholder next changes step: the deadline its
    /// window wakes at, so the steps take no frames in between.
    #[must_use]
    pub fn next_placeholder_step(&self, now: Instant) -> Option<Instant> {
        self.remote
            .iter()
            .filter_map(RemoteSection::placeholder)
            .filter_map(|clock| clock.next_step_at(now))
            .min()
    }

    /// Whether nothing was found anywhere, and nothing is still coming.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.local.is_empty()
            && !self.pending()
            && self.remote.iter().all(|section| section.groups.is_empty())
    }

    /// Every row shown, local first, as `(type, id)`: what a person sees
    /// from the top down.
    #[must_use]
    pub fn order(&self) -> Vec<(&'static str, String)> {
        self.local
            .iter()
            .flat_map(|group| &group.items)
            .chain(self.remote.iter().flat_map(RemoteSection::rows))
            .map(|item| (item.kind, item.id.clone()))
            .collect()
    }
}

/// How long a query took to show its results, from the moment it was set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryTimings {
    /// The local results: answered before the query was set.
    pub local: Option<Duration>,
    /// Each remote source's first rows, in the order the sources were given.
    pub remote_first: Vec<Option<Duration>>,
    /// Each remote source's whole answer.
    pub remote_done: Vec<Option<Duration>>,
}

/// One message from a request running off the UI thread.
struct Answer {
    source: usize,
    generation: u64,
    query: QuerySpec,
    event: AnswerEvent,
}

enum AnswerEvent {
    Page(Vec<Item>),
    Done,
    Failed(String),
}

/// One answer a remote source gave, kept by query.
struct CachedAnswer {
    key: String,
    query: QuerySpec,
    items: Vec<Item>,
    complete: bool,
    at: Instant,
}

/// One remote source and what is in flight to it.
struct RemoteSlot {
    source: Arc<dyn RemoteSource>,
    last_sent: Option<Instant>,
    /// When the current query goes out, if it has not yet.
    due: Option<Instant>,
    cancel: Option<Cancel>,
    warmed: Option<Instant>,
    cache: VecDeque<CachedAnswer>,
    /// The rows shown before this query's own answer: what an earlier,
    /// cached answer had that still matches.
    provisional: HashSet<(&'static str, String)>,
    /// What of this query's own answer has arrived.
    answered: Vec<Item>,
    sent_at: Option<Instant>,
}

impl RemoteSlot {
    fn cached(&self, key: &str) -> Option<&CachedAnswer> {
        self.cache.iter().find(|answer| answer.key == key)
    }

    /// The complete cached answer `query` goes on from, with the longest
    /// text.
    fn refined_from(&self, query: &QuerySpec) -> Option<&CachedAnswer> {
        self.cache
            .iter()
            .filter(|answer| answer.complete && query.refines(&answer.query))
            .max_by_key(|answer| answer.query.text.len())
    }

    fn keep(&mut self, query: &QuerySpec, items: &[Item], complete: bool, at: Instant) {
        let key = query.key();
        if let Some(at_index) = self.cache.iter().position(|answer| answer.key == key) {
            let mut answer = self.cache.remove(at_index).unwrap_or_else(|| CachedAnswer {
                key: key.clone(),
                query: query.clone(),
                items: Vec::new(),
                complete: false,
                at,
            });
            for item in items {
                if !answer.items.iter().any(|held| held.key() == item.key()) {
                    answer.items.push(item.clone());
                }
            }
            answer.complete |= complete;
            answer.at = at;
            self.cache.push_back(answer);
        } else {
            self.cache.push_back(CachedAnswer {
                key,
                query: query.clone(),
                items: items.to_vec(),
                complete,
                at,
            });
        }
        while self.cache.len() > CACHED_ANSWERS {
            self.cache.pop_front();
        }
    }
}

/// The app's local items and their index.
///
/// Items' fields are indexed here, a change at a time, in the frame it is
/// handed over. Their prose is indexed apart ([`ProseIndex`]): reading a
/// library's books takes longer than a frame, so it is built off the UI
/// thread, again only when the bodies themselves change, and searches
/// answer from the last one built until the next lands.
struct LocalIndex {
    index: SearchIndex,
    keys: HashMap<(&'static str, String), u64>,
    items: HashMap<u64, (Item, u64)>,
    next_key: u64,
    prose: Option<ProseIndex>,
    /// The bodies the newest prose index asked for holds, by item key.
    prose_wanted: HashMap<u64, usize>,
}

/// The prose of the items that have a body, indexed by item key: a match
/// names the segment it is in.
struct ProseIndex {
    /// Which build this is; a build older than one already in is dropped.
    build: u64,
    index: SearchIndex,
}

impl ProseIndex {
    fn built(build: u64, bodies: Vec<(u64, SearchBody)>) -> Self {
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(
            bodies
                .into_iter()
                .map(|(key, body)| Document::new(key).body(Arc::new(BodyText(body.shared())))),
        );
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index a view's prose");
        }
        Self { build, index }
    }
}

/// Where a body's text lives: what tells one body from another.
fn body_address(body: &SearchBody) -> usize {
    Arc::as_ptr(&body.shared()).cast::<()>().addr()
}

impl Default for LocalIndex {
    fn default() -> Self {
        Self {
            // Titles are names a person half remembers: typos match too.
            index: SearchIndex::new(Matching::LOOSE),
            keys: HashMap::new(),
            items: HashMap::new(),
            next_key: 0,
            prose: None,
            prose_wanted: HashMap::new(),
        }
    }
}

fn fingerprint(item: &Item) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    item.fields.hash(&mut hasher);
    item.found_by.hash(&mut hasher);
    // A body is the same body only while it shares one text.
    item.body.as_ref().map(body_address).hash(&mut hasher);
    hasher.finish()
}

fn weight(role: Option<FieldRole>) -> FieldWeight {
    match role {
        Some(FieldRole::Title) => FieldWeight::TITLE,
        Some(FieldRole::Subtitle) => FieldWeight::SUBTITLE,
        _ => FieldWeight::METADATA,
    }
}

impl LocalIndex {
    /// Take `items` as the whole local set: what changed is indexed again,
    /// what is gone leaves. Returns whether anything changed, and the
    /// bodies a new prose index is to hold when they changed.
    fn sync(
        &mut self,
        kinds: &[KindDecl],
        items: impl IntoIterator<Item = Item>,
    ) -> (bool, Option<Vec<(u64, SearchBody)>>) {
        let mut seen = HashSet::new();
        let mut changed = false;
        let mut bodies = Vec::new();
        let mut wanted = HashMap::new();
        for item in items {
            let print = fingerprint(&item);
            let key = if let Some(key) = self.keys.get(&(item.kind, item.id.clone())) {
                *key
            } else {
                let key = self.next_key;
                self.next_key = self.next_key.wrapping_add(1);
                self.keys.insert((item.kind, item.id.clone()), key);
                key
            };
            seen.insert(key);
            if let Some(body) = &item.body {
                wanted.insert(key, body_address(body));
                bodies.push((key, body.clone()));
            }
            if self.items.get(&key).is_some_and(|(_, held)| *held == print) {
                continue;
            }
            let kind = kinds.iter().find(|kind| kind.kind == item.kind);
            let document = item
                .fields
                .iter()
                .filter_map(|(field, value)| match kind {
                    Some(kind) => kind
                        .field_of(field)
                        .filter(|decl| decl.searchable)
                        .map(|decl| (value.as_str(), weight(decl.role))),
                    None => Some((value.as_str(), FieldWeight::METADATA)),
                })
                .chain(
                    item.found_by
                        .iter()
                        .map(|(text, weight)| (text.as_str(), field_weight(*weight))),
                )
                .fold(Document::new(key), |document, (value, weight)| {
                    document.field(value, weight)
                });
            self.index.upsert(document);
            self.items.insert(key, (item, print));
            changed = true;
        }
        let gone: Vec<u64> = self
            .items
            .keys()
            .filter(|key| !seen.contains(*key))
            .copied()
            .collect();
        for key in gone {
            if let Some((item, _)) = self.items.remove(&key) {
                self.keys.remove(&(item.kind, item.id));
            }
            self.index.remove(key);
            changed = true;
        }
        if changed && let Err(error) = self.index.commit() {
            tracing::warn!(%error, "could not index a view's local items");
        }
        let prose = (wanted != self.prose_wanted).then(|| {
            self.prose_wanted = wanted;
            bodies
        });
        (changed, prose)
    }

    /// Answer from `prose` from now on, unless a newer build is in.
    fn land_prose(&mut self, prose: ProseIndex) -> bool {
        if self
            .prose
            .as_ref()
            .is_some_and(|held| held.build > prose.build)
        {
            return false;
        }
        self.prose = Some(prose);
        true
    }

    /// The local items that match `query`, best first. A query of chips
    /// alone lists everything they admit, by title.
    fn search(&self, kinds: &[KindDecl], query: &QuerySpec) -> Vec<Item> {
        if query.text.trim().is_empty() {
            if query.chips.is_empty() {
                return Vec::new();
            }
            let mut found: Vec<(String, &Item)> = self
                .items
                .values()
                .map(|(item, _)| item)
                .filter(|item| query.admits(item))
                .map(|item| (tilo_search::fold(title_of(kinds, item)), item))
                .collect();
            found.sort_by(|(a_title, a), (b_title, b)| {
                a_title.cmp(b_title).then_with(|| a.id.cmp(&b.id))
            });
            return found.into_iter().map(|(_, item)| item.clone()).collect();
        }
        let mut seen = HashSet::new();
        // Matches in fields first, then in prose, as one index would rank
        // them.
        let prose = self.prose.as_ref().map(|prose| {
            prose
                .index
                .search(&query.text, Page::first(LOCAL_CANDIDATES))
                .hits
        });
        self.index
            .search(&query.text, Page::first(LOCAL_CANDIDATES))
            .hits
            .into_iter()
            .chain(prose.into_iter().flatten())
            .filter_map(|hit| {
                let (item, _) = self.items.get(&hit.key)?;
                Some(match hit.segment {
                    Some(segment) => located(kinds, item, segment, &query.text)?,
                    None => item.clone(),
                })
            })
            .filter(|item| query.admits(item))
            .filter(|item| seen.insert((item.kind, item.id.clone())))
            .collect()
    }
}

/// The hit a match in `segment` of `item`'s body is: the location the body
/// names there, as an item of the location's type, carrying the filter
/// values of the item it is in and the passage the match is in. `None`
/// when the body does not locate the segment or no type is the location's.
fn located(kinds: &[KindDecl], item: &Item, segment: usize, text: &str) -> Option<Item> {
    let body = item.body.as_ref()?.text();
    if !body.locates(segment) {
        return None;
    }
    let mut location = Item::from_entry(kinds, &body.location(segment)?)?;
    let decl = kinds.iter().find(|kind| kind.kind == location.kind)?;
    for (key, value) in &item.fields {
        let filters = decl
            .field_of(key)
            .is_some_and(|field| field.filter.is_some());
        if filters && location.value(key).is_none() {
            location.fields.push((*key, value.clone()));
        }
    }
    if let Some(field) = decl
        .fields
        .iter()
        .find(|field| field.role == Some(FieldRole::Excerpt))
    {
        let passage = segment_text(body, segment);
        location = location.with(field.key, excerpt(&passage, text));
    }
    Some(location)
}

/// The prose of `segment`, its pieces joined.
fn segment_text(body: &dyn tilo_resource::SearchText, segment: usize) -> String {
    let mut text = String::new();
    for at in 0..body.piece_count(segment) {
        if let Some(piece) = body.piece(segment, at) {
            if piece.space_before && !text.is_empty() {
                text.push(' ');
            }
            text.push_str(piece.text);
        }
    }
    text
}

/// How far back from where it would start an excerpt looks for a word's
/// start.
const EXCERPT_REACH: usize = 96;

/// How many characters an excerpt shows in all.
const EXCERPT_LENGTH: usize = 180;

/// The part of `passage` around the first word of `query` it holds, the
/// match in its middle, cut at word boundaries and marked where it was cut:
/// what a hit inside prose shows of it.
#[must_use]
pub fn excerpt(passage: &str, query: &str) -> String {
    let chars: Vec<(usize, char)> = passage.char_indices().collect();
    // The passage folded a character at a time, each folded character
    // pointing back at the one it came from.
    let mut folded = String::new();
    let mut origin = Vec::new();
    for (at, (_, ch)) in chars.iter().enumerate() {
        tilo_search::fold_char(*ch, &mut |folded_char| {
            folded.push(folded_char);
            origin.extend(std::iter::repeat_n(at, folded_char.len_utf8()));
        });
    }
    let (found, word_len) = query
        .split_whitespace()
        .map(tilo_search::fold)
        .filter(|word| !word.is_empty() && !word.contains(':'))
        .find_map(|word| {
            let byte = folded.find(&word)?;
            Some((*origin.get(byte)?, word.chars().count()))
        })
        .unwrap_or((0, 0));
    // The match in the middle, or as near it as the passage's ends allow.
    let before = EXCERPT_LENGTH.saturating_sub(word_len) / 2;
    let mut start = found
        .saturating_sub(before)
        .min(chars.len().saturating_sub(EXCERPT_LENGTH));
    let planned = start;
    // Back to the word's start, unless the word runs on for long.
    while start > 0
        && planned.saturating_sub(start) < EXCERPT_REACH
        && !chars
            .get(start.saturating_sub(1))
            .is_some_and(|(_, ch)| ch.is_whitespace())
    {
        start = start.saturating_sub(1);
    }
    let mut end = start.saturating_add(EXCERPT_LENGTH).min(chars.len());
    while end < chars.len() && !chars.get(end).is_some_and(|(_, ch)| ch.is_whitespace()) {
        end = end.saturating_add(1);
    }
    let byte = |at: usize| chars.get(at).map_or(passage.len(), |(byte, _)| *byte);
    let mut shown = passage
        .get(byte(start)..byte(end))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if start > 0 {
        shown.insert(0, '\u{2026}');
    }
    if end < chars.len() {
        shown.push('\u{2026}');
    }
    shown
}

/// The value of `item`'s title field, as its type declares it; its first
/// field when the type declares none.
fn title_of<'a>(kinds: &[KindDecl], item: &'a Item) -> &'a str {
    let title = kinds
        .iter()
        .find(|kind| kind.kind == item.kind)
        .and_then(|kind| {
            kind.fields
                .iter()
                .find(|field| field.role == Some(FieldRole::Title))
        })
        .and_then(|field| item.value(field.key));
    title.unwrap_or_else(|| item.fields.first().map_or("", |(_, value)| value.as_str()))
}

/// `items` as groups, one per type in the order of `kinds`, keeping their
/// order within a type; at most `limit` items a group.
fn grouped(kinds: &[KindDecl], items: Vec<Item>, limit: usize) -> Vec<ResultGroup> {
    let mut groups: Vec<ResultGroup> = Vec::new();
    for item in items {
        match groups.iter_mut().find(|group| group.kind == item.kind) {
            Some(group) if group.items.len() < limit => group.items.push(item),
            Some(group) => group.more = group.more.saturating_add(1),
            None => {
                let decl = kinds.iter().find(|kind| kind.kind == item.kind);
                groups.push(ResultGroup {
                    kind: item.kind,
                    label: decl.map_or_else(|| item.kind.to_owned(), |kind| kind.plural.clone()),
                    form: decl.map_or(Form::Row, |kind| kind.form),
                    items: vec![item],
                    more: 0,
                });
            }
        }
    }
    let order = |kind: &str| {
        kinds
            .iter()
            .position(|decl| decl.kind == kind)
            .unwrap_or(usize::MAX)
    };
    groups.sort_by_key(|group| order(group.kind));
    groups
}

/// The query of one query view: its local index, its remote sources, and
/// what it has found so far.
pub struct QueryEngine {
    kinds: Vec<KindDecl>,
    local: LocalIndex,
    remotes: Vec<RemoteSlot>,
    open: bool,
    results: QueryResults,
    timings: QueryTimings,
    started: Option<Instant>,
    held: bool,
    background: Background,
    wake: WakeHandle,
    send: Sender<Answer>,
    answers: Receiver<Answer>,
    /// How many prose indexes have been asked for.
    prose_builds: u64,
    prose_send: Sender<ProseIndex>,
    prose_built: Receiver<ProseIndex>,
    /// The app's own index, answering with the local items.
    source: Option<Arc<dyn LocalSource>>,
}

impl QueryEngine {
    /// An engine for items of `kinds`, which also order the groups. It runs
    /// remote requests on the executor current now and wakes the window it
    /// is made in when they answer.
    #[must_use]
    pub fn new(kinds: Vec<KindDecl>) -> Self {
        let (send, answers) = channel();
        let (prose_send, prose_built) = channel();
        Self {
            kinds,
            local: LocalIndex::default(),
            remotes: Vec::new(),
            open: false,
            results: QueryResults::default(),
            timings: QueryTimings::default(),
            started: None,
            held: false,
            background: Background::current(),
            wake: WakeHandle::new(),
            send,
            answers,
            prose_builds: 0,
            prose_send,
            prose_built,
            source: None,
        }
    }

    /// Answer with `source` too, beside the local items; `None` stops. What
    /// it finds for the open query joins the rows after those shown.
    pub fn set_local_source(&mut self, source: Option<Arc<dyn LocalSource>>) {
        self.source = source;
        if self.open {
            self.add_found();
        }
    }

    /// What the local items and the app's own index find for `query`, the
    /// items first, each once.
    fn found_locally(&self, query: &QuerySpec) -> Vec<Item> {
        let mut found = self.local.search(&self.kinds, query);
        if let Some(source) = &self.source
            && !query.text.trim().is_empty()
        {
            let mut seen: HashSet<(&'static str, String)> = found
                .iter()
                .map(|item| (item.kind, item.id.clone()))
                .collect();
            found.extend(
                source
                    .search(query, LOCAL_CANDIDATES)
                    .into_iter()
                    .filter(|item| query.admits(item))
                    .filter(|item| seen.insert((item.kind, item.id.clone()))),
            );
        }
        found
    }

    /// Ask `source` too, in a section below the local results.
    #[must_use]
    pub fn with_remote(mut self, source: Arc<dyn RemoteSource>) -> Self {
        self.remotes.push(RemoteSlot {
            source,
            last_sent: None,
            due: None,
            cancel: None,
            warmed: None,
            cache: VecDeque::new(),
            provisional: HashSet::new(),
            answered: Vec::new(),
            sent_at: None,
        });
        self
    }

    /// Run remote requests on `background` instead (a test's pumped
    /// executor).
    #[must_use]
    pub fn with_background(mut self, background: Background) -> Self {
        self.background = background;
        self
    }

    /// The types the engine knows.
    #[must_use]
    pub fn kinds(&self) -> &[KindDecl] {
        &self.kinds
    }

    /// Wake `wake`'s window when a remote answer arrives or a request falls
    /// due, in place of the one current when the engine was made.
    pub fn set_waker(&mut self, wake: WakeHandle) {
        self.wake = wake;
    }

    /// Take `items` as the app's whole local set; only what changed is
    /// indexed again. The rows an open query shows follow the change where
    /// they stand: a row whose item changed shows it changed (liked now), a
    /// row whose item left or no longer matches goes, and nothing new comes
    /// in above the rows below it until the query changes. Liking a
    /// catalogue song in the results brings it and its album into the
    /// library, and neither may push the row the person just pressed from
    /// under the pointer.
    ///
    /// Prose ([`Item::body`]) is indexed off the UI thread when the bodies
    /// change; the matches in it join the open query's rows when it lands
    /// ([`Self::poll`]), after the rows shown.
    pub fn sync_local(&mut self, items: impl IntoIterator<Item = Item>) {
        let (changed, prose) = self.local.sync(&self.kinds, items);
        if let Some(bodies) = prose {
            self.build_prose(bodies);
        }
        if !changed || !self.open {
            return;
        }
        let fresh: HashMap<(&'static str, String), Item> = self
            .found_locally(&self.results.query)
            .into_iter()
            .map(|item| ((item.kind, item.id.clone()), item))
            .collect();
        for group in &mut self.results.local {
            group.items = group
                .items
                .iter()
                .filter_map(|item| fresh.get(&(item.kind, item.id.clone())).cloned())
                .collect();
        }
        self.results.local.retain(|group| !group.items.is_empty());
    }

    /// Index `bodies` off the UI thread, or here when nothing runs work in
    /// the background.
    fn build_prose(&mut self, bodies: Vec<(u64, SearchBody)>) {
        self.prose_builds = self.prose_builds.wrapping_add(1);
        let build = self.prose_builds;
        let send = self.prose_send.clone();
        let wake = self.wake.clone();
        let spawned = self.background.spawn("view-prose-index", {
            let bodies = bodies.clone();
            move || {
                let _ = send.send(ProseIndex::built(build, bodies));
                wake.wake();
            }
        });
        if spawned.is_err() {
            self.land_prose(ProseIndex::built(build, bodies));
        }
    }

    /// Answer from `prose`, and add what it finds for the open query after
    /// the rows shown: a row the person sees never moves for it.
    fn land_prose(&mut self, prose: ProseIndex) -> bool {
        if !self.local.land_prose(prose) || !self.open {
            return false;
        }
        self.add_found()
    }

    /// Add what the open query finds now and did not before after the rows
    /// shown: a row the person sees never moves for it.
    fn add_found(&mut self) -> bool {
        let shown: HashSet<(&'static str, String)> = self
            .results
            .local
            .iter()
            .flat_map(|group| &group.items)
            .map(|item| (item.kind, item.id.clone()))
            .collect();
        let found: Vec<Item> = self
            .found_locally(&self.results.query)
            .into_iter()
            .filter(|item| !shown.contains(&(item.kind, item.id.clone())))
            .collect();
        if found.is_empty() {
            return false;
        }
        for group in grouped(&self.kinds, found, GROUP_LIMIT) {
            match self
                .results
                .local
                .iter_mut()
                .find(|held| held.kind == group.kind)
            {
                Some(held) => {
                    let room = GROUP_LIMIT.saturating_sub(held.items.len());
                    let extra = group.items.len().saturating_sub(room);
                    held.items.extend(group.items.into_iter().take(room));
                    held.more = held.more.saturating_add(group.more).saturating_add(extra);
                }
                None => self.results.local.push(group),
            }
        }
        true
    }

    /// The person focused the search field: open the remote sources'
    /// connections before the first character.
    pub fn warm(&mut self) {
        let now = self.background.now();
        for slot in &mut self.remotes {
            if slot
                .warmed
                .is_some_and(|at| now.saturating_duration_since(at) < WARM_FOR)
            {
                continue;
            }
            slot.warmed = Some(now);
            let source = Arc::clone(&slot.source);
            if self
                .background
                .spawn("view-query-warm", move || source.warm())
                .is_err()
            {
                tracing::debug!("no background work here: a remote source stays cold");
            }
        }
    }

    /// Ask `query`. The local results are in [`Self::results`] when this
    /// returns; remote ones follow through [`Self::poll`]. Asking what is
    /// asked already changes nothing. Returns whether the results changed.
    pub fn set_query(&mut self, query: QuerySpec) -> bool {
        if self.open && query.key() == self.results.query.key() {
            if query != self.results.query {
                // The same question typed another way: chips picked rather
                // than typed keep their look.
                self.results.query = query;
                return true;
            }
            return false;
        }
        let now = self.background.now();
        self.open = true;
        self.started = Some(now);
        self.results.generation = self.results.generation.wrapping_add(1);
        self.results.query = query;
        self.timings = QueryTimings {
            local: None,
            remote_first: vec![None; self.remotes.len()],
            remote_done: vec![None; self.remotes.len()],
        };
        // Prose indexed since the last poll answers this query already.
        while let Ok(prose) = self.prose_built.try_recv() {
            self.local.land_prose(prose);
        }
        let measure = ui_core::time::now();
        self.answer_locally();
        self.timings.local = Some(ui_core::time::now().saturating_duration_since(measure));
        let mut sections = Vec::with_capacity(self.remotes.len());
        for at in 0..self.remotes.len() {
            sections.push(self.start_remote(at, now));
        }
        self.results.remote = sections;
        self.arm(now);
        true
    }

    /// The open query's filter chips, each value filter's values counted
    /// among what the query finds without that filter's own choice: the
    /// library searched again without it, and the remote rows shown.
    #[must_use]
    pub fn facet_offers(&self) -> Vec<super::chips::FacetOffer> {
        let query = &self.results.query;
        let remote: Vec<Item> = self
            .results
            .remote
            .iter()
            .flat_map(RemoteSection::rows)
            .cloned()
            .collect();
        super::chips::offers_counting(&self.kinds, query, &|key| {
            let mut wider = query.clone();
            wider.chips.retain(|chip| chip.key != key);
            let mut rows = if query.chips.iter().any(|chip| chip.key == key) {
                self.found_locally(&wider)
            } else {
                super::chips::result_rows(&self.results)
                    .into_iter()
                    .filter(|item| !remote.iter().any(|shown| shown.key() == item.key()))
                    .collect()
            };
            rows.extend(remote.iter().cloned());
            rows
        })
    }

    /// Stop: every request is cancelled and nothing is shown.
    pub fn close(&mut self) {
        for slot in &mut self.remotes {
            if let Some(cancel) = slot.cancel.take() {
                cancel.cancel();
            }
            slot.due = None;
            slot.provisional.clear();
            slot.answered.clear();
        }
        self.open = false;
        self.held = false;
        self.results.generation = self.results.generation.wrapping_add(1);
        self.results.query = QuerySpec::default();
        self.results.local.clear();
        self.results.remote.clear();
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What the query has found so far.
    #[must_use]
    pub fn results(&self) -> &QueryResults {
        &self.results
    }

    /// How long the open query took to show what it has.
    #[must_use]
    pub fn timings(&self) -> &QueryTimings {
        &self.timings
    }

    /// Whether the person points at, or has the keyboard in, a remote
    /// section. While they do, arriving rows only go on at the end of what is
    /// shown; when they stop, a finished answer is laid out in its order.
    pub fn hold(&mut self, held: bool) {
        if self.held == held {
            return;
        }
        self.held = held;
        if !held {
            for at in 0..self.remotes.len() {
                self.relayout(at);
            }
        }
    }

    /// Send what is due and take in what arrived. Returns whether the
    /// results changed. Call every frame while a query is open; the engine
    /// wakes the window when something arrives or falls due.
    pub fn poll(&mut self) -> bool {
        let now = self.background.now();
        let mut changed = false;
        for at in 0..self.remotes.len() {
            if self
                .remotes
                .get(at)
                .and_then(|slot| slot.due)
                .is_some_and(|due| due <= now)
            {
                self.dispatch(at, now);
            }
        }
        while let Ok(answer) = self.answers.try_recv() {
            changed |= self.take(answer, now);
        }
        while let Ok(prose) = self.prose_built.try_recv() {
            changed |= self.land_prose(prose);
        }
        self.arm(now);
        changed
    }

    fn answer_locally(&mut self) {
        let found = self.found_locally(&self.results.query);
        self.results.local = grouped(&self.kinds, found, GROUP_LIMIT);
    }

    /// The section `at` starts with for the query just set, and its request
    /// scheduled.
    fn start_remote(&mut self, at: usize, now: Instant) -> RemoteSection {
        let query = self.results.query.clone();
        let local: HashSet<(&'static str, String)> = self
            .results
            .local
            .iter()
            .flat_map(|group| &group.items)
            .map(|item| (item.kind, item.id.clone()))
            .collect();
        let kinds = self.kinds.clone();
        let Some(slot) = self.remotes.get_mut(at) else {
            return RemoteSection::unasked(String::new(), String::new());
        };
        if let Some(cancel) = slot.cancel.take() {
            cancel.cancel();
        }
        slot.provisional.clear();
        slot.answered.clear();
        slot.due = None;
        slot.sent_at = None;
        let mut section = RemoteSection::unasked(slot.source.id().to_owned(), slot.source.label());
        if !slot.source.accepts(&query) {
            return section;
        }
        let not_local = |item: &&Item| !local.contains(&(item.kind, item.id.clone()));
        if let Some(cached) = slot.cached(&query.key()).filter(|cached| cached.complete) {
            let items: Vec<Item> = cached.items.iter().filter(not_local).cloned().collect();
            section.groups = grouped(&kinds, items, GROUP_LIMIT);
            if now.saturating_duration_since(cached.at) < FRESH {
                section.state = Some(Arrival::Ready(()));
                if let Some(first) = self.timings.remote_first.get_mut(at) {
                    *first = Some(Duration::ZERO);
                }
                if let Some(done) = self.timings.remote_done.get_mut(at) {
                    *done = Some(Duration::ZERO);
                }
                return section;
            }
            slot.provisional = section
                .rows()
                .map(|item| (item.kind, item.id.clone()))
                .collect();
        } else if let Some(earlier) = slot.refined_from(&query) {
            let items: Vec<Item> = earlier
                .items
                .iter()
                .filter(not_local)
                .filter(|item| {
                    query.admits(item)
                        && query.text_admits(item, kinds.iter().find(|kind| kind.kind == item.kind))
                })
                .cloned()
                .collect();
            section.groups = grouped(&kinds, items, GROUP_LIMIT);
            slot.provisional = section
                .rows()
                .map(|item| (item.kind, item.id.clone()))
                .collect();
        }
        section.ask(slot.source.origin(), now);
        let ready = slot
            .last_sent
            .map_or(now, |sent| sent.checked_add(THROTTLE).unwrap_or(now));
        slot.due = Some(ready.max(now));
        section
    }

    /// Send the open query to source `at`, cancelling what it was asked
    /// before.
    fn dispatch(&mut self, at: usize, now: Instant) {
        let generation = self.results.generation;
        let query = self.results.query.clone();
        let Some(slot) = self.remotes.get_mut(at) else {
            return;
        };
        slot.due = None;
        if let Some(cancel) = slot.cancel.take() {
            cancel.cancel();
        }
        let cancel = Cancel::default();
        slot.cancel = Some(cancel.clone());
        slot.last_sent = Some(now);
        slot.sent_at = Some(now);
        let source = Arc::clone(&slot.source);
        let send = self.send.clone();
        let wake = self.wake.clone();
        let spawned = self.background.spawn("view-query-remote", move || {
            let tell = |event: AnswerEvent| {
                let _ = send.send(Answer {
                    source: at,
                    generation,
                    query: query.clone(),
                    event,
                });
                wake.wake();
            };
            let result = source.search(&query, &cancel, &mut |items| {
                tell(AnswerEvent::Page(items));
            });
            tell(match result {
                Ok(()) => AnswerEvent::Done,
                Err(error) => AnswerEvent::Failed(error.0),
            });
        });
        if spawned.is_err()
            && let Some(section) = self.results.remote.get_mut(at)
        {
            section.settle(Err(RemoteError(String::new())), now);
        }
    }

    /// Take in one answer. Returns whether the results changed.
    fn take(&mut self, answer: Answer, now: Instant) -> bool {
        let current = self.open && answer.generation == self.results.generation;
        let Some(slot) = self.remotes.get_mut(answer.source) else {
            return false;
        };
        // Every answer is kept, a cancelled query's too: typing back to it
        // shows it at once.
        match &answer.event {
            AnswerEvent::Page(items) => slot.keep(&answer.query, items, false, now),
            AnswerEvent::Done => slot.keep(&answer.query, &[], true, now),
            AnswerEvent::Failed(_) => {}
        }
        if !current {
            return false;
        }
        let since = self
            .started
            .map(|started| now.saturating_duration_since(started));
        let at = answer.source;
        match answer.event {
            AnswerEvent::Page(items) => {
                if let Some(first) = self.timings.remote_first.get_mut(at)
                    && first.is_none()
                {
                    *first = since;
                }
                slot.answered.extend(items.iter().cloned());
                self.merge(at, items);
            }
            AnswerEvent::Done => {
                slot.cancel = None;
                if let Some(done) = self.timings.remote_done.get_mut(at) {
                    *done = since;
                }
                if let Some(section) = self.results.remote.get_mut(at) {
                    section.settle(Ok(()), now);
                }
                self.relayout(at);
            }
            AnswerEvent::Failed(message) => {
                slot.cancel = None;
                if let Some(section) = self.results.remote.get_mut(at) {
                    section.settle(Err(RemoteError(message)), now);
                }
            }
        }
        true
    }

    /// The local rows, which no remote row repeats.
    fn local_keys(&self) -> HashSet<(&'static str, String)> {
        self.results
            .local
            .iter()
            .flat_map(|group| &group.items)
            .map(|item| (item.kind, item.id.clone()))
            .collect()
    }

    /// Put arriving `items` into section `at`: after what is shown while it
    /// is held, else in their order.
    fn merge(&mut self, at: usize, items: Vec<Item>) {
        let local = self.local_keys();
        let held = self.held;
        let kinds = self.kinds.clone();
        let Some(section) = self.results.remote.get_mut(at) else {
            return;
        };
        let fresh: Vec<Item> = items
            .into_iter()
            .filter(|item| !local.contains(&(item.kind, item.id.clone())) && !section.holds(item))
            .collect();
        if fresh.is_empty() {
            return;
        }
        if held {
            append(section, fresh, &kinds);
        } else {
            let mut all: Vec<Item> = section.rows().cloned().collect();
            all.extend(fresh);
            section.groups = grouped(&kinds, all, GROUP_LIMIT);
        }
    }

    /// Lay section `at` out in the order of its own answer, once that is in
    /// and nobody holds it: the earlier answer's rows it did not repeat
    /// leave.
    fn relayout(&mut self, at: usize) {
        if self.held {
            return;
        }
        let local = self.local_keys();
        let kinds = self.kinds.clone();
        let (Some(slot), Some(section)) = (self.remotes.get(at), self.results.remote.get_mut(at))
        else {
            return;
        };
        if !section.is_answered() {
            return;
        }
        let mut seen = HashSet::new();
        let items: Vec<Item> = slot
            .answered
            .iter()
            .filter(|item| !local.contains(&(item.kind, item.id.clone())))
            .filter(|item| seen.insert((item.kind, item.id.clone())))
            .cloned()
            .collect();
        let laid_out = grouped(&kinds, items, GROUP_LIMIT);
        if laid_out != section.groups {
            section.groups = laid_out;
        }
    }

    /// Wake the window when the next request falls due.
    fn arm(&self, now: Instant) {
        if let Some(due) = self.remotes.iter().filter_map(|slot| slot.due).min() {
            if due <= now {
                self.wake.wake();
            } else {
                self.background.wake_at(due, self.wake.clone());
            }
        }
    }
}

/// Put `items` after everything `section` shows: into its last group when
/// they are of its type, else in groups of their own after it.
fn append(section: &mut RemoteSection, items: Vec<Item>, kinds: &[KindDecl]) {
    for group in grouped(kinds, items, usize::MAX) {
        match section.groups.last_mut() {
            Some(last) if last.kind == group.kind => last.items.extend(group.items),
            _ => section.groups.push(group),
        }
    }
}
