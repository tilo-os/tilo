//! The `YouTube` queries Music and Tube make, each compiled once.
//!
//! rustypipe's queries are generic (`query: S` where `S: AsRef<str>`) and
//! async. An app that awaits one instantiates the query's whole request and
//! response path in its own binary: the request builder, the response
//! models' deserializers and the futures that drive them, about 800 KB of
//! code in each of Music and Tube, although rustypipe itself is in
//! `libtilo_youtube_engine.so`. Each function here is one query with its
//! types fixed, returned as a boxed future, so that code is compiled in this
//! crate, which the engine library re-exports, and the app keeps a call
//! through the future's vtable.
//!
//! The functions are `#[inline(never)]`: a call inlined into the app would
//! instantiate the query there again.

use std::future::Future;
use std::pin::Pin;

use rustypipe::client::RustyPipeQuery;
use rustypipe::error::Error;
use rustypipe::model::paginator::Paginator;
use rustypipe::model::{
    AlbumItem, Channel, Comment, MusicAlbum, MusicArtist, MusicCharts, MusicItem, MusicPlaylist,
    MusicSearchResult, MusicSearchSuggestion, Playlist, SearchResult, TrackItem, VideoDetails,
    VideoItem, VideoPlayer, YouTubeItem,
};
use rustypipe::param::Country;
use rustypipe::param::search_filter::SearchFilter;

/// One query in flight: it borrows the query handle and its arguments.
pub type Query<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + 'a>>;

/// `YouTube` Music tracks matching `query`.
#[must_use]
#[inline(never)]
pub fn music_search_tracks<'a>(
    q: &'a RustyPipeQuery,
    query: &'a str,
) -> Query<'a, MusicSearchResult<TrackItem>> {
    Box::pin(q.music_search_tracks(query))
}

/// `YouTube` Music's top results for `query`: tracks, albums, artists and
/// playlists together.
#[must_use]
#[inline(never)]
pub fn music_search_main<'a>(
    q: &'a RustyPipeQuery,
    query: &'a str,
) -> Query<'a, MusicSearchResult<MusicItem>> {
    Box::pin(q.music_search_main(query))
}

/// What `YouTube` Music suggests for the partly typed `query`.
#[must_use]
#[inline(never)]
pub fn music_search_suggestion<'a>(
    q: &'a RustyPipeQuery,
    query: &'a str,
) -> Query<'a, MusicSearchSuggestion> {
    Box::pin(q.music_search_suggestion(query))
}

/// The player response of a video: its streams and their URLs.
#[must_use]
#[inline(never)]
pub fn player<'a>(q: &'a RustyPipeQuery, video_id: &'a str) -> Query<'a, VideoPlayer> {
    Box::pin(q.player(video_id))
}

/// `YouTube` Music's charts, for `country` or the query's own.
#[must_use]
#[inline(never)]
pub fn music_charts(q: &RustyPipeQuery, country: Option<Country>) -> Query<'_, MusicCharts> {
    Box::pin(q.music_charts(country))
}

/// The albums `YouTube` Music lists as new releases.
#[must_use]
#[inline(never)]
pub fn music_new_albums(q: &RustyPipeQuery) -> Query<'_, Vec<AlbumItem>> {
    Box::pin(q.music_new_albums())
}

/// An artist's page; with `all_albums`, every album rather than the first
/// few.
#[must_use]
#[inline(never)]
pub fn music_artist<'a>(
    q: &'a RustyPipeQuery,
    artist_id: &'a str,
    all_albums: bool,
) -> Query<'a, MusicArtist> {
    Box::pin(q.music_artist(artist_id, all_albums))
}

/// An album and its tracks.
#[must_use]
#[inline(never)]
pub fn music_album<'a>(q: &'a RustyPipeQuery, album_id: &'a str) -> Query<'a, MusicAlbum> {
    Box::pin(q.music_album(album_id))
}

/// A `YouTube` Music playlist and its tracks.
#[must_use]
#[inline(never)]
pub fn music_playlist<'a>(q: &'a RustyPipeQuery, playlist_id: &'a str) -> Query<'a, MusicPlaylist> {
    Box::pin(q.music_playlist(playlist_id))
}

/// Videos, playlists and channels matching `query`.
#[must_use]
#[inline(never)]
pub fn search<'a>(q: &'a RustyPipeQuery, query: &'a str) -> Query<'a, SearchResult<YouTubeItem>> {
    Box::pin(q.search::<YouTubeItem, _>(query))
}

/// What matches `query` under `filter`.
#[must_use]
#[inline(never)]
pub fn search_filter<'a>(
    q: &'a RustyPipeQuery,
    query: &'a str,
    filter: &'a SearchFilter,
) -> Query<'a, SearchResult<YouTubeItem>> {
    Box::pin(q.search_filter::<YouTubeItem, _>(query, filter))
}

/// A playlist and its first videos.
#[must_use]
#[inline(never)]
pub fn playlist<'a>(q: &'a RustyPipeQuery, playlist_id: &'a str) -> Query<'a, Playlist> {
    Box::pin(q.playlist(playlist_id))
}

/// A video's details: its title, channel, description, the videos
/// recommended beside it and the first page of its comments.
#[must_use]
#[inline(never)]
pub fn video_details<'a>(q: &'a RustyPipeQuery, video_id: &'a str) -> Query<'a, VideoDetails> {
    Box::pin(q.video_details(video_id))
}

/// A channel and its newest videos.
#[must_use]
#[inline(never)]
pub fn channel_videos<'a>(
    q: &'a RustyPipeQuery,
    channel_id: &'a str,
) -> Query<'a, Channel<Paginator<VideoItem>>> {
    Box::pin(q.channel_videos(channel_id))
}

/// The videos `YouTube` lists as trending.
#[must_use]
#[inline(never)]
pub fn trending(q: &RustyPipeQuery) -> Query<'_, Vec<VideoItem>> {
    Box::pin(q.trending())
}

/// The page of comments after `page`, or `None` when `page` was the last.
#[must_use]
#[inline(never)]
pub fn next_comments<'a>(
    page: &'a Paginator<Comment>,
    q: &'a RustyPipeQuery,
) -> Query<'a, Option<Paginator<Comment>>> {
    Box::pin(page.next(q))
}
