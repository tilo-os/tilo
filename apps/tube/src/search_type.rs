//! Tube's search source: [`model_entries`] are what the host-wide search
//! finds in Tube, each anchored at the video, creator, playlist or local
//! watch-history entry the window's opener shows ([`opened`]).

use ui::tilo_resource::SearchEntry;

const MAX_INDEXED_ENTITIES: usize = 100;
/// The videos, creators, playlists and watch history Tube shows, as search
/// entries, each anchored at what it opens. Kept to
/// the same 100-item ceiling as the published rows, so every hit stays
/// activatable; no entry gains authority merely because it is searchable.
pub fn model_entries(model: &crate::model::TubeModel) -> Vec<SearchEntry> {
    let dearrow = model.settings.dearrow_titles_on();
    let mut entries = Vec::new();
    for video in model.videos.iter().take(MAX_INDEXED_ENTITIES) {
        entries.push(
            SearchEntry::new(
                video.display_title(dearrow),
                format!("{} {}", video.title, video.id),
            )
            .anchored(video_anchor(&video.id))
            .grouped("Videos")
            .subtitled(video.channel_name.clone())
            .with_icon("play"),
        );
    }
    for channel in model.channels.iter().take(MAX_INDEXED_ENTITIES) {
        entries.push(
            SearchEntry::new(channel.name.clone(), channel.id.clone())
                .anchored(format!("tube.channel:{}", channel.id))
                .grouped("Creators")
                .subtitled(channel.handle.clone())
                .with_icon("users"),
        );
    }
    for playlist in model.playlists.iter().take(MAX_INDEXED_ENTITIES) {
        entries.push(
            SearchEntry::new(playlist.title.clone(), playlist.id.clone())
                .anchored(format!("tube.playlist:{}", playlist.id))
                .grouped("Playlists")
                .subtitled(playlist.channel_name.clone())
                .with_icon("list-video"),
        );
    }
    for video_id in model.history.iter().take(MAX_INDEXED_ENTITIES) {
        let Some(video) = model.video(video_id) else {
            continue;
        };
        entries.push(
            SearchEntry::new(
                video.display_title(dearrow),
                format!("{} {video_id}", video.title),
            )
            .anchored(format!("tube.history:{video_id}"))
            .grouped("Watch history")
            .subtitled(video.channel_name.clone())
            .with_icon("history"),
        );
    }
    entries
}

/// The row that searches `YouTube` for what the platform search was asked,
/// `query`: the catalogue only holds what has loaded, so without it a
/// search for anything else would find nothing. Leads the results, so
/// Enter searches; `None` for an empty query.
pub fn query_entry(query: &str) -> Option<SearchEntry> {
    let query = query.trim();
    if query.is_empty() || query.chars().any(char::is_control) {
        return None;
    }
    let title = crate::i18n::t_args("tube-search-for", &[crate::i18n::arg("query", query)]);
    Some(
        SearchEntry::new(title, query)
            .anchored(format!("{SEARCH_ANCHOR}{query}"))
            .grouped("YouTube")
            .with_icon("search")
            .boosted(QUERY_BOOST),
    )
}

/// The rows of the videos `YouTube` answered for `query` ([`crate::suggest`]),
/// in its order, under [`query_entry`]'s row; each opens its video. Found
/// by the query they answer, whatever their titles say.
pub fn suggested_entries(
    query: &str,
    videos: &[crate::model::Video],
    dearrow: bool,
) -> Vec<SearchEntry> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    videos
        .iter()
        .take(crate::suggest::SUGGESTED_VIDEOS)
        .zip(1..)
        .map(|(video, place)| {
            SearchEntry::new(video.display_title(dearrow), query)
                .anchored(video_anchor(&video.id))
                .grouped("YouTube")
                .subtitled(video.channel_name.clone())
                .with_icon("play")
                .boosted(QUERY_BOOST - place)
        })
        .collect()
}

/// The anchor of the hit that opens the video `id`.
pub fn video_anchor(id: &str) -> String {
    format!("tube.video:{id}")
}

/// The anchor prefix of [`query_entry`]'s row.
const SEARCH_ANCHOR: &str = "tube.search:";
/// How far [`query_entry`]'s row floats over the videos the query matches.
const QUERY_BOOST: i32 = 1000;

/// What opening the anchor `anchor` runs: the video (from the catalogue or
/// the watch history), the creator or the playlist it names.
pub fn opened(anchor: &str) -> Option<crate::actions::TubeEffect> {
    use crate::actions::TubeEffect;
    let (kind, id) = anchor.split_once(':')?;
    let id = id.to_owned();
    match kind {
        "tube.video" | "tube.history" => Some(TubeEffect::OpenVideo { video: id }),
        "tube.channel" => Some(TubeEffect::OpenCreator { creator: id }),
        "tube.playlist" => Some(TubeEffect::OpenPlaylist { playlist: id }),
        "tube.search" => Some(TubeEffect::Search { query: id }),
        _ => None,
    }
}

/// The scheme a launch names a search under: `tube:search?q=<query>`,
/// the query percent-encoded.
pub const SEARCH_URI_PREFIX: &str = "tube:search?q=";

/// The query a launch asks Tube to search for, decoded; `None` for any
/// other launch, and for an empty or malformed query.
pub fn launch_search(arguments: &[String]) -> Option<String> {
    arguments.iter().find_map(|arg| {
        let prefix = arg.get(..SEARCH_URI_PREFIX.len())?;
        if !prefix.eq_ignore_ascii_case(SEARCH_URI_PREFIX) || arg.len() > 8 * 1024 {
            return None;
        }
        let query = percent_decode(arg.get(SEARCH_URI_PREFIX.len()..)?)?;
        let query = query.trim();
        (!query.is_empty() && !query.chars().any(char::is_control)).then(|| query.to_owned())
    })
}

/// `text` with `+` as a space and `%XX` as its byte; `None` when an escape
/// is malformed or the bytes are not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.bytes();
    while let Some(byte) = rest.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let high = char::from(rest.next()?).to_digit(16)?;
                let low = char::from(rest.next()?).to_digit(16)?;
                bytes.push(u8::try_from(high * 16 + low).ok()?);
            }
            _ => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_names_a_search_by_its_uri() {
        assert_eq!(
            launch_search(&["tube:search?q=cats".into()]).as_deref(),
            Some("cats")
        );
        assert_eq!(
            launch_search(&["tube:search?q=lo-fi+k%C3%B6ln%20mix".into()]).as_deref(),
            Some("lo-fi köln mix")
        );
        for value in [
            "tube:search?q=",
            "tube:search?q=%2",
            "tube:video?v=1",
            "cats",
        ] {
            assert_eq!(launch_search(&[value.into()]), None, "{value}");
        }
    }

    #[test]
    fn a_typed_query_offers_to_search_youtube_for_it() {
        let entry = query_entry(" lo-fi mix ").expect("a query gets its row");
        assert_eq!(entry.title, "Search YouTube for “lo-fi mix”");
        let provider =
            ui::framework::testing::search_source::<crate::resource::Library>(vec![entry]);
        let hits = ui::search::SearchProvider::search(&provider, "lo-fi mix");
        let hit = hits.first().expect("the query finds its own row");
        assert_eq!(
            opened(&hit.action_id),
            Some(crate::actions::TubeEffect::Search {
                query: "lo-fi mix".into()
            })
        );
        assert!(query_entry("  ").is_none());
        assert!(query_entry("a\u{7}b").is_none());
    }

    #[test]
    fn host_search_hits_open_videos_playlists_and_history() {
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
        let provider = ui::framework::testing::search_source::<crate::resource::Library>(
            model_entries(&model),
        );
        for (query, resource, open) in [
            ("compositor", "tube.video:", "open-video"),
            ("watchlist", "tube.playlist:", "open-playlist"),
            ("compositor", "tube.history:", "open-video"),
        ] {
            assert!(
                ui::search::SearchProvider::search(&provider, query)
                    .iter()
                    .any(|hit| {
                        hit.action_id.starts_with(resource)
                            && opened(&hit.action_id).is_some_and(|effect| {
                                ui::tilo_resource::Effect::id(&effect) == open
                            })
                    }),
                "{query:?} finds a {resource} hit that opens with {open}"
            );
        }
    }
}
