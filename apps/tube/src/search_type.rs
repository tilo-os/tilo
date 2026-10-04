//! Tube in the platform search.
//!
//! [`TubeSearchProvider`] answers the palette (Ctrl+K, `/`, the header's
//! magnifier) from two sources: the catalogue Tube already holds, at once,
//! and — in the live app — `YouTube` itself, as a slow page that arrives when
//! the background worker's search lands ([`LiveSearch`]). Each hit names the
//! exact resource action that opens it, and says what it is with a search
//! row, so the platform draws videos as media rows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, PagePoll, PendingToken,
    SearchCatalog, SearchCursor, SearchPage, SearchProvider, SearchQuery, SearchResult, SearchRow,
};

use crate::model::{TubeModel, Video};
use crate::util::{clock, fnv1a};

/// A unique, stable id for a searchable result type so the framework can
/// deduplicate providers and persist per-type preferences.
pub const VIDEO_TYPE_ID: &str = "tube.video";

/// Stable id for the channel result type.
pub const CHANNEL_TYPE_ID: &str = "tube.channel";

/// Stable id for playlists contributed to host-wide search.
pub const PLAYLIST_TYPE_ID: &str = "tube.playlist";

/// Stable id for device-local watch-history entries contributed to search.
pub const HISTORY_TYPE_ID: &str = "tube.history";

const TUBE_SEARCH_KIND: &str = "Tube";
const MAX_INDEXED_ENTITIES: usize = 100;
/// Live answers kept for queries the palette may still ask about.
const MAX_LIVE_ANSWERS: usize = 16;

/// A static descriptor of a result type an app contributes to global search.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchableType {
    /// Stable id (e.g. [`VIDEO_TYPE_ID`]).
    pub id: &'static str,
    /// Human, localisable display name for the palette section header.
    pub display_name: &'static str,
    /// Whether hits of this type carry a thumbnail (so the palette reserves
    /// image space).
    pub has_thumbnail: bool,
}

pub fn video_searchable_type() -> SearchableType {
    SearchableType {
        id: VIDEO_TYPE_ID,
        display_name: "YouTube video",
        has_thumbnail: true,
    }
}

pub fn channel_searchable_type() -> SearchableType {
    SearchableType {
        id: CHANNEL_TYPE_ID,
        display_name: "YouTube channel",
        has_thumbnail: true,
    }
}

pub fn playlist_searchable_type() -> SearchableType {
    SearchableType {
        id: PLAYLIST_TYPE_ID,
        display_name: "YouTube playlist",
        has_thumbnail: true,
    }
}

pub fn history_searchable_type() -> SearchableType {
    SearchableType {
        id: HISTORY_TYPE_ID,
        display_name: "Tube watch history",
        has_thumbnail: true,
    }
}

/// A video hit: a media row with its creator and length.
fn video_result(group: &str, video: &Video, title: &str, address: ActionAddress) -> SearchResult {
    let mut row = SearchRow::media(title).byline(&video.channel_name);
    if video.duration > 0 && !video.live {
        row = row.duration(clock(f64::from(video.duration)));
    }
    SearchResult::for_action(group, title, address)
        .with_subtitle(&video.channel_name)
        .with_icon(Icon::Play)
        .with_row(row)
}

/// The live answer for one query: its hits and the resources they open, for
/// hits beyond the bounded resource tree Tube publishes.
#[derive(Clone, Default)]
struct LiveAnswer {
    results: Vec<SearchResult>,
    resources: Vec<Resource>,
}

#[derive(Default)]
struct LiveSearchState {
    /// The newest query the palette is waiting on, for the root to fetch.
    wanted: Option<String>,
    /// Queries asked for and not yet answered, by token.
    waiting: HashMap<u64, String>,
    answers: HashMap<String, LiveAnswer>,
    /// Answer order, oldest first, to bound `answers`.
    order: Vec<String>,
}

/// The bridge between the platform search and Tube's background worker: the
/// provider records what the palette asks for, the root fetches it and
/// answers here, and the palette's next poll picks the answer up.
#[derive(Clone, Default)]
pub struct LiveSearch(Rc<RefCell<LiveSearchState>>);

impl LiveSearch {
    fn token(query: &str) -> PendingToken {
        PendingToken::new(u64::from(fnv1a(query)))
    }

    /// The query to fetch, once.
    pub fn take_wanted(&self) -> Option<String> {
        self.0.borrow_mut().wanted.take()
    }

    /// Record the hits for `query` (empty when the search failed).
    pub fn answer(&self, query: &str, answer: (Vec<SearchResult>, Vec<Resource>)) {
        let mut state = self.0.borrow_mut();
        let key = query.trim().to_string();
        state.waiting.retain(|_, waiting| *waiting != key);
        state.order.retain(|q| *q != key);
        state.order.push(key.clone());
        state.answers.insert(
            key,
            LiveAnswer {
                results: answer.0,
                resources: answer.1,
            },
        );
        while state.order.len() > MAX_LIVE_ANSWERS {
            let oldest = state.order.remove(0);
            state.answers.remove(&oldest);
        }
    }

    /// Settle every query still waiting with no hits (the network is gone).
    pub fn answer_all_waiting(&self) {
        let waiting: Vec<String> = self.0.borrow().waiting.values().cloned().collect();
        for query in waiting {
            self.answer(&query, (Vec::new(), Vec::new()));
        }
    }

    fn answered(&self, query: &str) -> Option<LiveAnswer> {
        self.0.borrow().answers.get(query.trim()).cloned()
    }

    /// Ask for `query`, returning the token its answer will come under.
    fn ask(&self, query: &str) -> PendingToken {
        let key = query.trim().to_string();
        let token = Self::token(&key);
        let mut state = self.0.borrow_mut();
        if !state.waiting.values().any(|q| *q == key) {
            state.waiting.insert(token.id(), key.clone());
            state.wanted = Some(key);
        }
        token
    }

    fn query_for(&self, token: &PendingToken) -> Option<String> {
        self.0
            .borrow()
            .waiting
            .get(&token.id())
            .cloned()
            .or_else(|| {
                let state = self.0.borrow();
                state
                    .answers
                    .keys()
                    .find(|q| &Self::token(q) == token)
                    .cloned()
            })
    }

    fn resource(&self, address: &ActionAddress) -> Option<Resource> {
        self.0.borrow().answers.values().find_map(|answer| {
            answer
                .resources
                .iter()
                .find(|resource| resource.id.0 == address.resource)
                .cloned()
        })
    }
}

/// The hits the live backend returned for `query` (in its order), with the
/// resources that open them.
pub fn live_results(model: &TubeModel, query: &str) -> (Vec<SearchResult>, Vec<Resource>) {
    let mut results = Vec::new();
    let mut resources = Vec::new();
    if model.search_hits_query != query {
        return (results, resources);
    }
    let dearrow = model.settings.dearrow_titles_on();
    for video in model
        .search_display(query)
        .into_iter()
        .take(MAX_INDEXED_ENTITIES)
    {
        let address = ActionAddress::new(format!("tube.video:{}", video.id), "open");
        results.push(video_result(
            "Videos",
            video,
            video.display_title(dearrow),
            address,
        ));
        resources.extend(crate::actions::video_resource(model, &video.id, false));
    }
    for channel in model.search_creators_display(query) {
        let address = ActionAddress::new(format!("tube.channel:{}", channel.id), "open");
        results.push(
            SearchResult::for_action("Creators", &channel.name, address)
                .with_subtitle(&channel.handle)
                .with_icon(Icon::Users),
        );
        resources.extend(crate::actions::channel_resource(model, &channel.id));
    }
    for playlist in model.search_playlists_display(query) {
        let address = ActionAddress::new(format!("tube.playlist:{}", playlist.id), "open");
        results.push(
            SearchResult::for_action("Playlists", &playlist.title, address)
                .with_subtitle(&playlist.channel_name)
                .with_icon(Icon::ListVideo),
        );
        resources.extend(crate::actions::playlist_resource(model, &playlist.id));
    }
    (results, resources)
}

/// Tube's search provider. The catalogue Tube holds answers at once; with a
/// [`LiveSearch`] attached, `YouTube` answers the rest as a slow page.
#[derive(Clone)]
pub struct TubeSearchProvider {
    catalog: SearchCatalog,
    live: Option<LiveSearch>,
}

impl TubeSearchProvider {
    pub fn new(model: &TubeModel) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(TUBE_SEARCH_KIND),
            live: None,
        };
        let dearrow = model.settings.dearrow_titles_on();
        for video in model.videos.iter().take(MAX_INDEXED_ENTITIES) {
            let address = ActionAddress::new(format!("tube.video:{}", video.id), "open");
            let result = video_result(
                "Videos",
                video,
                video.display_title(dearrow),
                address.clone(),
            );
            let searchable = format!("{}\0{}\0{}", video.title, video.channel_name, video.id);
            let _ = provider
                .catalog
                .upsert(address.resource, searchable, result);
        }
        for channel in model.channels.iter().take(MAX_INDEXED_ENTITIES) {
            let address = ActionAddress::new(format!("tube.channel:{}", channel.id), "open");
            let result = SearchResult::for_action("Creators", &channel.name, address.clone())
                .with_subtitle(&channel.handle)
                .with_icon(Icon::Users);
            let searchable = format!("{}\0{}\0{}", channel.name, channel.handle, channel.id);
            let _ = provider
                .catalog
                .upsert(address.resource, searchable, result);
        }
        for playlist in model.playlists.iter().take(MAX_INDEXED_ENTITIES) {
            let address = ActionAddress::new(format!("tube.playlist:{}", playlist.id), "open");
            let result = SearchResult::for_action("Playlists", &playlist.title, address.clone())
                .with_subtitle(&playlist.channel_name)
                .with_icon(Icon::ListVideo);
            let searchable = format!(
                "{}\0{}\0{}",
                playlist.title, playlist.channel_name, playlist.id
            );
            let _ = provider
                .catalog
                .upsert(address.resource, searchable, result);
        }
        for video_id in model.history.iter().take(MAX_INDEXED_ENTITIES) {
            let Some(video) = model.video(video_id) else {
                continue;
            };
            let address = ActionAddress::new(format!("tube.history:{video_id}"), "open");
            let result = video_result(
                "Watch history",
                video,
                video.display_title(dearrow),
                address.clone(),
            )
            .with_icon(Icon::History);
            let searchable = format!("{}\0{}\0{}", video.title, video.channel_name, video_id);
            let _ = provider
                .catalog
                .upsert(address.resource, searchable, result);
        }
        provider
    }

    /// Also ask `YouTube`, through `live`.
    #[must_use]
    pub fn with_live(mut self, live: LiveSearch) -> Self {
        self.live = Some(live);
        self
    }

    /// The live hits not already among `local`.
    fn fresh(local: &[SearchResult], live: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
        let known: Vec<String> = local.iter().map(SearchResult::anchor).collect();
        live.into_iter()
            .filter(|hit| !known.contains(&hit.anchor()))
            .take(limit)
            .collect()
    }
}

impl SearchProvider for TubeSearchProvider {
    fn name(&self) -> &str {
        TUBE_SEARCH_KIND
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalog.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog
            .query_page(query, None, DEFAULT_SEARCH_CANDIDATE_LIMIT, limit)
            .results
    }

    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let mut page = self
            .catalog
            .query_page(query.text(), cursor, work_budget, result_limit);
        page.results.retain(|hit| query.in_scope(&hit.anchor()));
        let text = query.text().trim();
        let Some(live) = &self.live else {
            return page;
        };
        if cursor.is_some() || page.next.is_some() || text.is_empty() {
            return page;
        }
        match live.answered(text) {
            Some(answer) => {
                let room = result_limit.saturating_sub(page.results.len());
                let mut more = Self::fresh(&page.results, answer.results, room);
                more.retain(|hit| query.in_scope(&hit.anchor()));
                page.results.append(&mut more);
                SearchPage::of(page.results, None)
            }
            None => SearchPage::pending(page.results, live.ask(text)),
        }
    }

    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        let Some(live) = &self.live else {
            return PagePoll::Ready(SearchPage::empty());
        };
        let Some(query) = live.query_for(token) else {
            return PagePoll::Ready(SearchPage::empty());
        };
        match live.answered(&query) {
            Some(answer) => {
                let local = self.search_limited(&query, DEFAULT_SEARCH_RESULT_LIMIT);
                PagePoll::Ready(SearchPage::of(
                    Self::fresh(&local, answer.results, DEFAULT_SEARCH_RESULT_LIMIT),
                    None,
                ))
            }
            None => PagePoll::Pending,
        }
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        self.live.as_ref()?.resource(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_type_descriptors_are_stable() {
        let t = video_searchable_type();
        assert_eq!(t.id, "tube.video");
        assert!(t.has_thumbnail);
    }

    #[test]
    fn the_catalogue_answers_with_canonical_actions_and_media_rows() {
        let mut model = crate::sample::model();
        let video_id = model.videos[0].id.clone();
        model.history.push(video_id.clone());
        model.playlists.push(crate::model::Playlist {
            id: "playlist-rust".into(),
            title: "Rust watchlist".into(),
            channel_id: model.videos[0].channel_id.clone(),
            channel_name: model.videos[0].channel_name.clone(),
            thumbnail_url: String::new(),
            seed: "playlist-rust".into(),
            video_count: 1,
            video_ids: vec![video_id],
        });
        let provider = TubeSearchProvider::new(&model);
        for (query, resource) in [
            ("compositor", "tube.video:"),
            ("watchlist", "tube.playlist:"),
            ("compositor", "tube.history:"),
            ("Low Level", "tube.channel:"),
        ] {
            assert!(
                provider.search(query).iter().any(|hit| {
                    hit.action_address().is_some_and(|address| {
                        address.resource.starts_with(resource) && address.action == "open"
                    })
                }),
                "{query:?} exposes a canonical {resource} open action"
            );
        }
        assert!(
            provider
                .search("compositor")
                .iter()
                .filter(|hit| hit.anchor().starts_with("tube.video:"))
                .all(|hit| matches!(hit.row, Some(SearchRow::Media { .. }))),
            "videos are media rows"
        );
    }

    #[test]
    fn youtube_answers_as_a_slow_page() {
        let mut model = crate::sample::model();
        let live = LiveSearch::default();
        let provider = TubeSearchProvider::new(&model).with_live(live.clone());

        let page = provider.search_page(&SearchQuery::new("donut"), None, 64, 64);
        let token = page.pending.expect("the YouTube page is on its way");
        assert_eq!(live.take_wanted().as_deref(), Some("donut"));
        assert_eq!(live.take_wanted(), None, "asked for once");
        assert!(matches!(provider.poll_page(&token), PagePoll::Pending));

        // The worker lands a result that the catalogue did not hold.
        let mut found = model.videos[0].clone();
        found.id = "found-on-youtube".into();
        found.title = "A donut from the network".into();
        model.videos.push(found);
        model.search_hits_query = "donut".into();
        model.search_hits = vec!["found-on-youtube".into()];
        live.answer("donut", live_results(&model, "donut"));

        let PagePoll::Ready(page) = provider.poll_page(&token) else {
            panic!("the answer is ready");
        };
        let hit = page
            .results
            .iter()
            .find(|hit| hit.anchor() == "tube.video:found-on-youtube")
            .expect("the network hit is in the page");
        let address = hit.action_address().expect("an action hit");
        assert!(
            provider.resolve_action_resource(&address).is_some(),
            "the hit opens even outside the bounded resource tree"
        );
    }
}
