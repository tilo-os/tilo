//! Bounded Ctrl+K projection for the active video and its decoder metadata.
//!
//! Search records canonical resource/action addresses only. Activating a hit
//! therefore goes through the same fresh action door as a click or an agent,
//! rather than retaining a closure over a previously-opened file.

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::actions::PlayerResourceCtx;

pub const VIDEO_PLAYER_SEARCH_KIND: &str = "Video Player";

#[derive(Clone)]
pub struct VideoPlayerSearchProvider {
    catalog: SearchCatalog,
}

impl VideoPlayerSearchProvider {
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "all canonical search addresses live together so UI activation cannot drift from resource publication"
    )]
    pub fn new(ctx: &PlayerResourceCtx<'_>) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(VIDEO_PLAYER_SEARCH_KIND),
        };
        let _ = provider.catalog.upsert(
            "video-player.application",
            "video player\0open video\0file picker",
            SearchResult::for_action(
                VIDEO_PLAYER_SEARCH_KIND,
                "Open video",
                ActionAddress::new("video-player.application", "open"),
            )
            .with_icon(Icon::Play),
        );
        if ctx.media.has_media {
            let _ = provider.catalog.upsert(
                "video-player.transport.current",
                format!(
                    "{}\0playback\0seek\0volume\0{}×\0repeat",
                    ctx.title, ctx.settings.rate
                ),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    ctx.title,
                    ActionAddress::new("video-player.transport.current", "reveal"),
                )
                .with_subtitle("Current video")
                .with_icon(Icon::Play),
            );
        }
        let source_id = ctx.media.source_id.as_deref();
        for track in ctx.audio_tracks {
            let Some(source_id) = source_id else { continue };
            let _ = provider.catalog.upsert(
                crate::actions::audio_resource_id(source_id, &track.stream_id),
                format!("{}\0audio track\0select audio", track.label),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &track.label,
                    ActionAddress::new(
                        crate::actions::audio_resource_id(source_id, &track.stream_id),
                        format!(
                            "select-audio-{}",
                            crate::actions::identity_token_for_action(&track.stream_id)
                        ),
                    ),
                )
                .with_subtitle(if track.selected {
                    "Selected"
                } else {
                    "Audio track"
                })
                .with_icon(Icon::Play),
            );
        }
        for track in ctx.subtitle_tracks {
            let Some(source_id) = source_id else { continue };
            let _ = provider.catalog.upsert(
                crate::actions::subtitle_resource_id(source_id, &track.stream_id),
                format!("{}\0subtitle track\0captions", track.label),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &track.label,
                    ActionAddress::new(
                        crate::actions::subtitle_resource_id(source_id, &track.stream_id),
                        format!(
                            "select-subtitle-{}",
                            crate::actions::identity_token_for_action(&track.stream_id)
                        ),
                    ),
                )
                .with_subtitle(if track.selected {
                    "Selected captions"
                } else {
                    "Subtitle track"
                })
                .with_icon(Icon::Play),
            );
        }
        for chapter in ctx.chapters {
            let Some(source_id) = source_id else { continue };
            let _ = provider.catalog.upsert(
                crate::actions::chapter_resource_id(source_id, &chapter.id),
                format!(
                    "{}\0chapter\0seek\0{:.1} seconds",
                    chapter.label, chapter.seconds
                ),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &chapter.label,
                    ActionAddress::new(
                        crate::actions::chapter_resource_id(source_id, &chapter.id),
                        format!(
                            "jump-to-{}",
                            crate::actions::identity_token_for_action(&chapter.id)
                        ),
                    ),
                )
                .with_subtitle(format!("{:.1}s", chapter.seconds))
                .with_icon(Icon::Play),
            );
        }
        provider
    }
}

impl SearchProvider for VideoPlayerSearchProvider {
    fn name(&self) -> &str {
        VIDEO_PLAYER_SEARCH_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog.search_limited(query, limit)
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ChapterState, MediaAvailability, PlaybackSettingsState, TrackState};

    #[test]
    fn unicode_metadata_is_cursor_paged_and_keeps_canonical_actions() {
        let audio = vec![TrackState {
            stream_id: "audio-munich".into(),
            index: 41,
            label: "München 夏".into(),
            selected: false,
        }];
        let chapters = vec![ChapterState {
            id: "intro".into(),
            label: "Résumé".into(),
            seconds: 4.0,
        }];
        let context = PlayerResourceCtx {
            title: "München 夏.mkv",
            media: MediaAvailability {
                has_media: true,
                source_id: Some("fixture://munich".into()),
                can_control_transport: true,
            },
            settings: PlaybackSettingsState {
                autoplay: true,
                loop_enabled: false,
                rate: 1.0,
            },
            audio_tracks: &audio,
            subtitle_tracks: &[],
            chapters: &chapters,
        };
        let provider = VideoPlayerSearchProvider::new(&context);
        let hits = provider.search("München");
        assert!(hits.iter().any(|result| {
            result.action_address()
                == Some(ActionAddress::new(
                    crate::actions::audio_resource_id("fixture://munich", "audio-munich"),
                    format!(
                        "select-audio-{}",
                        crate::actions::identity_token_for_action("audio-munich")
                    ),
                ))
        }));
        let page = provider.search_page("Résumé", None, 1, 1);
        assert!(
            page.inspected <= 1,
            "the posting list keeps page work bounded"
        );
        assert!(page.results.iter().any(|result| {
            result.action_address().is_some_and(|address| {
                address.action
                    == format!(
                        "jump-to-{}",
                        crate::actions::identity_token_for_action("intro")
                    )
            })
        }));
    }

    #[test]
    fn late_metadata_match_remains_cursor_paged() {
        let audio: Vec<_> = (0..5_000)
            .map(|index| TrackState {
                stream_id: format!("stream-{index}"),
                index,
                label: if index == 4_999 {
                    "Final Ωmega track".into()
                } else {
                    format!("Audio {index}")
                },
                selected: false,
            })
            .collect();
        let context = PlayerResourceCtx {
            title: "large.mkv",
            media: MediaAvailability {
                has_media: true,
                source_id: Some("fixture://large".into()),
                can_control_transport: true,
            },
            settings: PlaybackSettingsState {
                autoplay: true,
                loop_enabled: false,
                rate: 1.0,
            },
            audio_tracks: &audio,
            subtitle_tracks: &[],
            chapters: &[],
        };
        let provider = VideoPlayerSearchProvider::new(&context);
        let first = provider.search_page("Ωmega", None, 64, 4);
        assert!(first.inspected <= 64);
        let mut page = first;
        while page.next.is_some() && page.results.is_empty() {
            page = provider.search_page("Ωmega", page.next.as_ref(), 64, 4);
            assert!(page.inspected <= 64, "each page obeys its scan budget");
        }
        assert_eq!(
            page.results.len(),
            1,
            "late Unicode match is eventually found"
        );
    }
}
