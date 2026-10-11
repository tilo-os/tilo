//! The selected remote music catalogue as a slow platform-search source.
//!
//! Search belongs to the platform: Music draws no field. The library is
//! answered at once by [`crate::search_type::LibrarySearchProvider`]; the
//! remote catalogue answers here, asynchronously. [`LiveCatalogProvider`]
//! never blocks the palette: a query registers a request and returns a
//! pending page, the app sends the request to its streaming worker on its
//! next frame, and the palette polls the token until the worker's reply has
//! been turned into rows.
//!
//! A hit is anchored at a row ([`LiveHit::anchor`]): opening it plays a
//! song through its library row, or opens the page of an album, artist or
//! playlist the library does not hold through the row this provider
//! resolves.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use ui::icons::Icon;
use ui::search::{
    PagePoll, PendingToken, SearchCursor, SearchPage, SearchQuery, SearchResult, SearchRow,
};
use ui::tilo_actions::{ActionAddress, Resource};

use crate::backend::SearchBundle;

/// Most catalogue rows the provider resolves a hit's action on.
const MAX_FOUND: usize = 256;

/// What activating a live hit does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveHit {
    /// Play the song with its stable provider-specific library id.
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
}

impl LiveHit {
    /// The row the hit is anchored at: opening it plays a song (the
    /// catalogue's songs join the library as they arrive) or opens the page
    /// of an album, artist or playlist.
    pub fn anchor(&self) -> String {
        match self {
            Self::Track(id) => crate::actions::track_resource_id(id),
            Self::Album(id) => crate::actions::album_resource_id(id),
            Self::Artist(id) => crate::actions::artist_resource_id(id),
            Self::Playlist(id) => crate::actions::catalogue_playlist_resource_id(id),
        }
    }
}

/// The rows a catalogue reply becomes, grouped by kind.
pub fn rows_of(bundle: &SearchBundle) -> Vec<SearchResult> {
    let mut rows = Vec::new();
    for hit in &bundle.tracks {
        let id = crate::app::live_track_id(&hit.video_id);
        let mut row = SearchRow::media(&hit.title).byline(&hit.artist);
        if hit.duration_s > 0 {
            row = row.duration(ui::format::timecode(u64::from(hit.duration_s)));
        }
        rows.push(
            SearchResult::new("Songs", &hit.title, LiveHit::Track(id).anchor())
                .with_icon(Icon::Music)
                .with_row(row),
        );
    }
    for album in &bundle.albums {
        rows.push(
            SearchResult::new(
                "Albums",
                &album.title,
                LiveHit::Album(album.id.clone()).anchor(),
            )
            .with_icon(Icon::GalleryVerticalEnd)
            .with_subtitle(&album.artist),
        );
    }
    for artist in &bundle.artists {
        let mut row = SearchResult::new(
            "Artists",
            &artist.name,
            LiveHit::Artist(artist.id.clone()).anchor(),
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
            LiveHit::Playlist(playlist.id.clone()).anchor(),
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
    /// The labels of the albums, artists and playlists the catalogue
    /// answered with, by resource id, whose pages a hit opens.
    found: BTreeMap<String, String>,
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
        for row in rows {
            let anchor = row.anchor();
            if crate::actions::live_catalogue_resource(&anchor, &row.title).is_some() {
                if exchange.found.len() >= MAX_FOUND {
                    exchange.found.clear();
                }
                exchange.found.insert(anchor, row.title.clone());
            }
        }
    }

    /// Whether the catalogue answered with the album, artist or playlist
    /// `resource_id`, so its page may open though the library lacks it.
    pub fn found(&self, resource_id: &str) -> bool {
        self.lock().found.contains_key(resource_id)
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
        "Music catalogue"
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

    /// The page of an album, artist or playlist the catalogue answered
    /// with; a song's row is the library's.
    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        let label = self.lock().found.get(&address.resource)?.clone();
        crate::actions::live_catalogue_resource(&address.resource, &label)
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
    fn rows_say_what_each_hit_is_and_open_their_row() {
        let rows = rows_of(&bundle());
        let song = rows.first().expect("song row");
        assert_eq!(song.kind, "Songs");
        assert!(matches!(song.row, Some(SearchRow::Media { .. })));
        assert_eq!(song.anchor(), crate::actions::track_resource_id("yt:abc"));
        assert!(matches!(
            crate::actions::anchor_effect(&song.anchor()),
            Some(crate::actions::MusicEffect::Play { .. })
        ));
        let album = rows.get(1).expect("album row");
        assert_eq!(album.subtitle.as_deref(), Some("Aurora Skies"));
        assert_eq!(album.anchor(), crate::actions::album_resource_id("MPRE1"));
        let anchor = album.anchor();

        // The album's page is found once the catalogue answered with it.
        let live = LiveSearch::default();
        assert!(!live.found(&anchor));
        live.answer("glass", &rows);
        assert!(live.found(&anchor));
        assert_eq!(
            live.lock().found.get(&anchor).map(String::as_str),
            Some("Paper Boats")
        );
    }
    #[test]
    fn spotify_search_keeps_provider_identity_in_platform_actions() {
        let mut reply = bundle();
        reply.tracks[0].video_id = "spotify:track:1234567890123456789012".into();
        reply.albums[0].id = "spotify:album:2234567890123456789012".into();
        let rows = rows_of(&reply);
        assert_eq!(
            rows[0].anchor(),
            crate::actions::track_resource_id("spotify:track:1234567890123456789012")
        );
        let anchor = crate::actions::album_resource_id("spotify:album:2234567890123456789012");
        assert_eq!(rows[1].anchor(), anchor);
        let live = LiveSearch::default();
        live.answer("glass", &rows);
        assert_eq!(
            live.lock().found.get(&anchor).map(String::as_str),
            Some("Paper Boats"),
            "provider album is found through platform search"
        );
    }
}
