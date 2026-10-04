//! The `YouTube Music` catalogue as a slow platform-search source.
//!
//! Search belongs to the platform: Music draws no field. The library is
//! answered at once by [`crate::search_type::LibrarySearchProvider`]; the
//! remote catalogue answers here, asynchronously. [`LiveCatalogProvider`]
//! never blocks the palette: a query registers a request and returns a
//! pending page, the app sends the request to its streaming worker on its
//! next frame, and the palette polls the token until the worker's reply has
//! been turned into rows.
//!
//! A hit's activation token is an app-local id ([`LiveHit`]) that the app
//! resolves in `SearchSource::activate_search_result`: a song plays, an
//! album, artist or playlist opens its page.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use ui::icons::Icon;
use ui::search::{
    PagePoll, PendingToken, SearchCursor, SearchPage, SearchQuery, SearchResult, SearchRow,
};

use crate::backend::SearchBundle;

/// The prefix of every live-catalogue activation id.
const HIT_PREFIX: &str = "music-live:";

/// What activating a live hit does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveHit {
    /// Play the song with this library id (`yt:<video id>`).
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
}

impl LiveHit {
    fn action_id(&self) -> String {
        match self {
            Self::Track(id) => format!("{HIT_PREFIX}track:{id}"),
            Self::Album(id) => format!("{HIT_PREFIX}album:{id}"),
            Self::Artist(id) => format!("{HIT_PREFIX}artist:{id}"),
            Self::Playlist(id) => format!("{HIT_PREFIX}playlist:{id}"),
        }
    }

    /// Parse an activation id this provider issued.
    pub fn parse(action_id: &str) -> Option<Self> {
        let rest = action_id.strip_prefix(HIT_PREFIX)?;
        let (kind, id) = rest.split_once(':')?;
        if id.is_empty() {
            return None;
        }
        let id = id.to_string();
        match kind {
            "track" => Some(Self::Track(id)),
            "album" => Some(Self::Album(id)),
            "artist" => Some(Self::Artist(id)),
            "playlist" => Some(Self::Playlist(id)),
            _ => None,
        }
    }
}

/// The rows a catalogue reply becomes, grouped by kind.
pub fn rows_of(bundle: &SearchBundle) -> Vec<SearchResult> {
    let mut rows = Vec::new();
    for hit in &bundle.tracks {
        let id = format!("yt:{}", hit.video_id);
        let mut row = SearchRow::media(&hit.title).byline(&hit.artist);
        if hit.duration_s > 0 {
            row = row.duration(ui::format::timecode(u64::from(hit.duration_s)));
        }
        rows.push(
            SearchResult::new("Songs", &hit.title, LiveHit::Track(id).action_id())
                .with_icon(Icon::Music)
                .with_row(row),
        );
    }
    for album in &bundle.albums {
        rows.push(
            SearchResult::new(
                "Albums",
                &album.title,
                LiveHit::Album(album.id.clone()).action_id(),
            )
            .with_icon(Icon::GalleryVerticalEnd)
            .with_subtitle(&album.artist),
        );
    }
    for artist in &bundle.artists {
        let mut row = SearchResult::new(
            "Artists",
            &artist.name,
            LiveHit::Artist(artist.id.clone()).action_id(),
        )
        .with_icon(Icon::Users);
        if artist.subscriber_count > 0 {
            row = row.with_subtitle(format!(
                "{} subscribers",
                ui::format::compact_count(artist.subscriber_count)
            ));
        }
        rows.push(row);
    }
    for playlist in &bundle.playlists {
        let mut row = SearchResult::new(
            "Playlists",
            &playlist.title,
            LiveHit::Playlist(playlist.id.clone()).action_id(),
        )
        .with_icon(Icon::List);
        if !playlist.subtitle.is_empty() {
            row = row.with_subtitle(&playlist.subtitle);
        }
        rows.push(row);
    }
    rows
}

/// What the provider and the app share: the queries waiting to be sent, the
/// tokens still awaited and the rows that answered them.
#[derive(Debug, Default)]
struct Exchange {
    /// Whether the catalogue can be reached at all (a hermetic screenshot or
    /// test never opens a socket).
    reachable: bool,
    next_token: u64,
    /// Queries the palette asked for that the app has not sent yet.
    asked: Vec<(u64, String)>,
    /// Tokens still awaited, with their query.
    waiting: BTreeMap<u64, String>,
    /// Answers ready for the palette, by token.
    answered: BTreeMap<u64, Vec<SearchResult>>,
}

/// The app's side of the exchange.
#[derive(Clone, Debug, Default)]
pub struct LiveSearch {
    exchange: Arc<Mutex<Exchange>>,
}

impl LiveSearch {
    fn lock(&self) -> MutexGuard<'_, Exchange> {
        self.exchange
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Allow (or forbid) queries to reach the catalogue.
    pub fn set_reachable(&self, reachable: bool) {
        self.lock().reachable = reachable;
    }

    /// The provider the platform search queries.
    pub fn provider(&self) -> LiveCatalogProvider {
        LiveCatalogProvider {
            exchange: Arc::clone(&self.exchange),
        }
    }

    /// The newest query the palette asked for since the last call, if any.
    /// Older queries are superseded: only the latest is worth asking, and
    /// tokens still waiting on another query finish with nothing.
    pub fn take_query(&self) -> Option<String> {
        let mut exchange = self.lock();
        let latest = exchange.asked.last().map(|(_, query)| query.clone())?;
        exchange.asked.clear();
        let stale: Vec<u64> = exchange
            .waiting
            .iter()
            .filter(|(_, query)| **query != latest)
            .map(|(token, _)| *token)
            .collect();
        for token in stale {
            exchange.waiting.remove(&token);
            exchange.answered.insert(token, Vec::new());
        }
        drop(exchange);
        Some(latest)
    }

    /// Whether a query is waiting to be sent or answered.
    pub fn is_busy(&self) -> bool {
        let exchange = self.lock();
        !exchange.asked.is_empty() || !exchange.waiting.is_empty()
    }

    /// Answer every token waiting on `query` with `rows`.
    pub fn answer(&self, query: &str, rows: &[SearchResult]) {
        let mut exchange = self.lock();
        let tokens: Vec<u64> = exchange
            .waiting
            .iter()
            .filter(|(_, asked)| asked.trim() == query.trim())
            .map(|(token, _)| *token)
            .collect();
        for token in tokens {
            exchange.waiting.remove(&token);
            exchange.answered.insert(token, rows.to_vec());
        }
    }

    /// The catalogue could not answer `query`: its tokens finish empty.
    pub fn fail(&self, query: &str) {
        self.answer(query, &[]);
    }
}

/// The platform-search provider for the remote catalogue. See the module
/// docs.
#[derive(Clone, Debug)]
pub struct LiveCatalogProvider {
    exchange: Arc<Mutex<Exchange>>,
}

impl LiveCatalogProvider {
    fn lock(&self) -> MutexGuard<'_, Exchange> {
        self.exchange
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ui::search::SearchProvider for LiveCatalogProvider {
    fn name(&self) -> &'static str {
        "YouTube Music"
    }

    /// The catalogue never answers synchronously; see [`Self::search_page`].
    fn search(&self, _query: &str) -> Vec<SearchResult> {
        Vec::new()
    }

    fn search_limited(&self, _query: &str, _limit: usize) -> Vec<SearchResult> {
        Vec::new()
    }

    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let text = query.text().trim();
        if cursor.is_some() || work_budget == 0 || result_limit == 0 || text.chars().count() < 2 {
            return SearchPage::empty();
        }
        let mut exchange = self.lock();
        if !exchange.reachable {
            return SearchPage::empty();
        }
        exchange.next_token = exchange.next_token.saturating_add(1);
        let token = exchange.next_token;
        exchange.asked.push((token, text.to_string()));
        exchange.waiting.insert(token, text.to_string());
        drop(exchange);
        SearchPage::pending(Vec::new(), PendingToken::new(token))
    }

    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        let mut exchange = self.lock();
        if let Some(rows) = exchange.answered.remove(&token.id()) {
            return PagePoll::Ready(SearchPage::of(rows, None));
        }
        if exchange.waiting.contains_key(&token.id()) {
            PagePoll::Pending
        } else {
            PagePoll::Ready(SearchPage::empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AlbumHit, AlbumKind, TrackHit};
    use ui::search::SearchProvider as _;

    fn bundle() -> SearchBundle {
        SearchBundle {
            tracks: vec![TrackHit {
                video_id: "abc".into(),
                title: "Glasslight".into(),
                artist: "Aurora Skies".into(),
                artist_id: None,
                album: None,
                album_id: None,
                duration_s: 215,
                cover_url: String::new(),
            }],
            albums: vec![AlbumHit {
                id: "MPRE1".into(),
                title: "Paper Boats".into(),
                artist: "Aurora Skies".into(),
                artist_id: None,
                year: 2021,
                kind: AlbumKind::Album,
                cover_url: String::new(),
            }],
            ..SearchBundle::default()
        }
    }

    #[test]
    fn a_query_waits_for_the_catalogue_and_never_blocks() {
        let live = LiveSearch::default();
        let provider = live.provider();
        let query = SearchQuery::new("glass");

        // Unreachable (hermetic): nothing pending, nothing asked.
        assert_eq!(
            provider.search_page(&query, None, 10, 10),
            SearchPage::empty()
        );
        assert!(live.take_query().is_none());

        live.set_reachable(true);
        let page = provider.search_page(&query, None, 10, 10);
        let token = page.pending.clone().expect("the catalogue answers later");
        assert!(page.results.is_empty());
        assert_eq!(provider.poll_page(&token), PagePoll::Pending);
        assert_eq!(live.take_query().as_deref(), Some("glass"));
        assert!(live.is_busy(), "the token is still awaited");

        let rows = rows_of(&bundle());
        live.answer("glass", &rows);
        let PagePoll::Ready(ready) = provider.poll_page(&token) else {
            panic!("answered tokens are ready");
        };
        assert_eq!(ready.results.len(), 2);
        assert!(!live.is_busy());
        assert_eq!(
            provider.poll_page(&token),
            PagePoll::Ready(SearchPage::empty()),
            "a token is answered once"
        );
    }

    #[test]
    fn rows_say_what_each_hit_is_and_activate_through_the_app() {
        let rows = rows_of(&bundle());
        let song = rows.first().expect("song row");
        assert_eq!(song.kind, "Songs");
        assert!(matches!(song.row, Some(SearchRow::Media { .. })));
        assert_eq!(
            LiveHit::parse(&song.action_id),
            Some(LiveHit::Track("yt:abc".into()))
        );
        let album = rows.get(1).expect("album row");
        assert_eq!(album.subtitle.as_deref(), Some("Aurora Skies"));
        assert_eq!(
            LiveHit::parse(&album.action_id),
            Some(LiveHit::Album("MPRE1".into()))
        );
        assert_eq!(LiveHit::parse("music-live:track:"), None);
        assert_eq!(LiveHit::parse("other:track:x"), None);
    }
}
