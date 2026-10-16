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
            .anchored(format!("tube.video:{}", video.id))
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
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
