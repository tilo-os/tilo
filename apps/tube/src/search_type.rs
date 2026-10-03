//! Tube search sources.
//!
//! [`TubeSearchProvider`] is the real [`ui::search::SearchProvider`] used by
//! the host-wide action palette. Its rows name the exact resource/action that
//! opens a video, creator, playlist, or local watch-history entry. The older
//! [`VideoSearchProvider`] remains the small, backend-facing projection used
//! by Tube's visual Ctrl+K overlay while it is open.

use crate::backend::{BackendResult, YtBackend};
use tilo_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

/// Hard projection budget for one provider query. The UI may render fewer;
/// global search must never materialise a backend-sized catalogue in one turn.
pub const MAX_SEARCH_HITS: usize = 50;

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

/// Bounded snapshot of the resources Tube currently publishes. Keeping the
/// catalogue and resource tree on the same 100-item ceiling means every
/// canonical hit remains activatable; no search result gains authority merely
/// because it was indexed.
#[derive(Clone)]
pub struct TubeSearchProvider {
    catalog: SearchCatalog,
}

impl TubeSearchProvider {
    pub fn new(model: &crate::model::TubeModel) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(TUBE_SEARCH_KIND),
        };
        for video in model.videos.iter().take(MAX_INDEXED_ENTITIES) {
            let address = ActionAddress::new(format!("tube.video:{}", video.id), "open");
            let result = SearchResult::for_action(
                "Videos",
                video.display_title(model.settings.dearrow_titles_on()),
                address.clone(),
            )
            .with_subtitle(&video.channel_name)
            .with_icon(Icon::Play);
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
            let result = SearchResult::for_action(
                "Watch history",
                video.display_title(model.settings.dearrow_titles_on()),
                address.clone(),
            )
            .with_subtitle(&video.channel_name)
            .with_icon(Icon::History);
            let searchable = format!("{}\0{}\0{}", video.title, video.channel_name, video_id);
            let _ = provider
                .catalog
                .upsert(address.resource, searchable, result);
        }
        provider
    }
}

impl CatalogSearchProvider for TubeSearchProvider {
    fn provider_name(&self) -> &str {
        TUBE_SEARCH_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

/// A single global-search result row contributed by Tube.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// [`VIDEO_TYPE_ID`] or [`CHANNEL_TYPE_ID`].
    pub type_id: &'static str,
    /// Video title (DeArrow-corrected at registration time if enabled), or
    /// channel name.
    pub title: String,
    /// Uploader / creator name, or the channel's own handle for a channel hit.
    pub creator: String,
    /// Duration in seconds (for the length pill the palette renders); 0 for
    /// a channel hit.
    pub length_secs: u32,
    /// Seed for Tube's deterministic thumbnail gradient — the palette can ask
    /// Tube to render it, so no image bytes cross the boundary.
    pub thumbnail_seed: String,
    /// Opaque deep link the framework round-trips back to Tube to open the
    /// result: `tube://watch/<video_id>` or `tube://channel/<channel_id>`.
    pub deep_link: String,
}

impl SearchHit {
    pub fn watch_link(video_id: &str) -> String {
        format!("tube://watch/{video_id}")
    }

    pub fn channel_link(channel_id: &str) -> String {
        format!("tube://channel/{channel_id}")
    }

    /// The app's router calls this when the framework activates a hit.
    pub fn parse_watch_link(link: &str) -> Option<String> {
        link.strip_prefix("tube://watch/")
            .map(std::string::ToString::to_string)
    }

    pub fn parse_channel_link(link: &str) -> Option<String> {
        link.strip_prefix("tube://channel/")
            .map(std::string::ToString::to_string)
    }
}

/// Turns a query into [`SearchHit`]s by delegating to whatever
/// [`crate::backend::YtBackend`] the build selected (offline sample by default,
/// rustypipe under `--features live`). The global-search framework owns ranking
/// across types; this provider only contributes Tube's rows.
pub struct VideoSearchProvider<'a> {
    backend: &'a dyn YtBackend,
    /// Whether `DeArrow` title correction is applied to contributed hits.
    dearrow: bool,
}

impl<'a> VideoSearchProvider<'a> {
    pub fn new(backend: &'a dyn YtBackend, dearrow: bool) -> Self {
        Self { backend, dearrow }
    }

    pub fn searchable_type(&self) -> SearchableType {
        video_searchable_type()
    }

    /// Contribute hits for `query`. Errors collapse to an empty contribution —
    /// a failing video search must not break OS-wide search.
    pub fn query(&self, query: &str) -> Vec<SearchHit> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let Ok(BackendResult::Videos(vids)) = self.backend.search(query) else {
            return Vec::new();
        };
        vids.into_iter()
            .take(MAX_SEARCH_HITS)
            .map(|v| SearchHit {
                type_id: VIDEO_TYPE_ID,
                title: v.display_title(self.dearrow).to_string(),
                creator: v.channel_name.clone(),
                length_secs: v.duration,
                thumbnail_seed: v.seed.clone(),
                deep_link: SearchHit::watch_link(&v.id),
            })
            .collect()
    }
}

/// Combined video + channel hits for the in-app Ctrl+K palette, ranked
/// videos-first. Channels are matched by name against the query over the given
/// model (the palette runs against Tube's in-memory model, which the offline
/// backend also sources from). Empty query yields nothing.
pub fn palette_hits(
    provider: &VideoSearchProvider<'_>,
    model: &crate::model::TubeModel,
    query: &str,
) -> Vec<SearchHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits = provider.query(q);
    let ql = q.to_lowercase();
    for ch in model
        .channels
        .iter()
        .take(MAX_SEARCH_HITS.saturating_sub(hits.len()))
    {
        if ch.name.to_lowercase().contains(&ql) || ch.handle.to_lowercase().contains(&ql) {
            hits.push(SearchHit {
                type_id: CHANNEL_TYPE_ID,
                title: ch.name.clone(),
                creator: ch.handle.clone(),
                length_secs: 0,
                thumbnail_seed: ch.seed.clone(),
                deep_link: SearchHit::channel_link(&ch.id),
            });
        }
    }
    hits.truncate(MAX_SEARCH_HITS);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::offline::OfflineBackend;
    use ui::search::SearchProvider;

    #[test]
    fn search_type_descriptor_and_deep_links_are_stable() {
        // Case `descriptor_is_stable`.
        {
            let t = video_searchable_type();
            assert_eq!(t.id, "tube.video");
            assert!(t.has_thumbnail);
        }
        // Case `deep_link_round_trips`.
        {
            let link = SearchHit::watch_link("vid_rust_async");
            assert_eq!(link, "tube://watch/vid_rust_async");
            assert_eq!(
                SearchHit::parse_watch_link(&link).as_deref(),
                Some("vid_rust_async")
            );
            assert!(SearchHit::parse_watch_link("https://evil").is_none());
        }
    }

    #[test]
    fn search_providers_contribute_canonical_video_and_channel_hits() {
        // Case `provider_contributes_video_hits`.
        {
            let backend = OfflineBackend::new();
            let provider = VideoSearchProvider::new(&backend, false);
            let hits = provider.query("rust");
            assert!(!hits.is_empty());
            assert!(hits.iter().all(|h| h.type_id == VIDEO_TYPE_ID));
            assert!(
                hits.iter()
                    .all(|h| h.deep_link.starts_with("tube://watch/"))
            );
            assert!(hits.iter().all(|h| !h.thumbnail_seed.is_empty()));
        }
        // Case `palette_hits_combine_videos_and_channels`.
        {
            let model = crate::sample::model();
            let backend = OfflineBackend::with_model(model.clone());
            let provider = VideoSearchProvider::new(&backend, false);
            // "Low Level" matches the channel "Low Level Learning".
            let hits = palette_hits(&provider, &model, "Low Level");
            assert!(
                hits.iter()
                    .any(|h| h.type_id == CHANNEL_TYPE_ID
                        && h.deep_link.starts_with("tube://channel/")),
                "channel results appear in the palette"
            );
        }
        // Case `palette_hits_empty_query_is_empty`.
        {
            let model = crate::sample::model();
            let backend = OfflineBackend::with_model(model.clone());
            let provider = VideoSearchProvider::new(&backend, false);
            assert!(palette_hits(&provider, &model, "   ").is_empty());
        }
        // Case `host_provider_uses_canonical_actions_for_video_playlist_and_history`.
        {
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
        }
        // Case `provider_applies_dearrow_when_enabled`.
        {
            let backend = OfflineBackend::new();
            // vid_rust_async has a DeArrow title in the sample.
            let plain = VideoSearchProvider::new(&backend, false).query("compositor");
            let dearrowed = VideoSearchProvider::new(&backend, true).query("compositor");
            let p = plain
                .iter()
                .find(|h| h.deep_link.contains("vid_rust_async"));
            let d = dearrowed
                .iter()
                .find(|h| h.deep_link.contains("vid_rust_async"));
            if let (Some(p), Some(d)) = (p, d) {
                assert_ne!(
                    p.title, d.title,
                    "DeArrow should change the contributed title"
                );
            }
        }
    }
}
