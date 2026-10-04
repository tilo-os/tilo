//! Bounded Ctrl+K projection for the active video and its decoder metadata.
//!
//! Search records canonical resource/action addresses only. Activating a hit
//! therefore goes through the same fresh action door as a click or an agent,
//! rather than retaining a closure over a previously-opened file.

use tilo_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

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
    pub fn new(ctx: &PlayerResourceCtx<'_>, shared: &crate::shared::SharedLibrary) -> Self {
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
        for item in &ctx.library.queue {
            let _ = provider.catalog.upsert(
                crate::actions::queue_item_resource_id(&item.id),
                format!("{}\0queue\0local video\0play", item.title),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &item.title,
                    ActionAddress::new(crate::actions::queue_item_resource_id(&item.id), "play"),
                )
                .with_subtitle(if item.current { "Playing now" } else { "Queue" })
                .with_icon(Icon::Play),
            );
        }
        for item in &ctx.library.recent {
            let _ = provider.catalog.upsert(
                crate::actions::recent_item_resource_id(&item.id),
                format!("{}\0recent history\0local video\0play again", item.title),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &item.title,
                    ActionAddress::new(crate::actions::recent_item_resource_id(&item.id), "play"),
                )
                .with_subtitle("Recent")
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
                .with_subtitle(ui::format::timecode(chapter.seconds.max(0.0) as u64))
                .with_icon(Icon::Play),
            );
        }
        for playlist in shared.playlists.iter().take(crate::shared::MAX_SHARED_ROWS) {
            let _ = provider.catalog.upsert(
                crate::actions::playlist_resource_id(&playlist.id),
                format!("{}\0shared playlist\0collaboration", playlist.name),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &playlist.name,
                    ActionAddress::new(crate::actions::playlist_resource_id(&playlist.id), "show"),
                )
                .with_subtitle("Shared playlist")
                .with_icon(Icon::ListVideo),
            );
        }
        for annotation in shared
            .annotations
            .iter()
            .take(crate::shared::MAX_SHARED_ROWS)
        {
            let time = ui::format::timecode(annotation.time_ms / 1000);
            let title = if annotation.text.is_empty() {
                format!("Bookmark at {time}")
            } else {
                annotation.text.clone()
            };
            let action = if ctx.media.has_media
                && ctx.media.can_control_transport
                && ctx.media.media_ref.as_deref() == Some(annotation.media_ref.as_str())
            {
                "jump"
            } else {
                "show"
            };
            let subtitle = if action == "jump" {
                format!("Jump to {time}")
            } else {
                format!("Bookmark at {time}")
            };
            let _ = provider.catalog.upsert(
                crate::actions::annotation_resource_id(&annotation.id),
                format!(
                    "{title}\0timestamp annotation\0bookmark\0{}",
                    annotation.time_ms
                ),
                SearchResult::for_action(
                    VIDEO_PLAYER_SEARCH_KIND,
                    &title,
                    ActionAddress::new(
                        crate::actions::annotation_resource_id(&annotation.id),
                        action,
                    ),
                )
                .with_subtitle(subtitle)
                .with_icon(Icon::Pin),
            );
        }
        provider
    }
}

impl CatalogSearchProvider for VideoPlayerSearchProvider {
    fn provider_name(&self) -> &str {
        VIDEO_PLAYER_SEARCH_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ChapterState, MediaAvailability, PlaybackSettingsState, TrackState};
    use ui::search::SearchProvider;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn library_metadata_search_is_cursor_paged_and_opaque() {
        // Case `unicode_metadata_is_cursor_paged_and_keeps_canonical_actions`.
        {
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
                    media_ref: Some("fixture-media://munich".into()),
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
                library: crate::actions::LocalLibraryState::default(),
            };
            let provider =
                VideoPlayerSearchProvider::new(&context, &crate::shared::SharedLibrary::default());
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
            let page = provider.search_page(&ui::search::SearchQuery::new("Résumé"), None, 1, 1);
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
        // Case `local_library_search_uses_opaque_resource_addresses_not_source_paths`.
        {
            let context = PlayerResourceCtx {
                title: "No media",
                media: MediaAvailability {
                    has_media: false,
                    source_id: None,
                    media_ref: None,
                    can_control_transport: true,
                },
                settings: PlaybackSettingsState {
                    autoplay: true,
                    loop_enabled: false,
                    rate: 1.0,
                },
                audio_tracks: &[],
                subtitle_tracks: &[],
                chapters: &[],
                library: crate::actions::LocalLibraryState {
                    queue: vec![crate::actions::LocalMediaItemState {
                        id: "local-queue-local-media-a1b2-open-0".into(),
                        title: "Family clip.mp4".into(),
                        current: false,
                    }],
                    recent: Vec::new(),
                },
            };
            let provider =
                VideoPlayerSearchProvider::new(&context, &crate::shared::SharedLibrary::default());
            let hit = provider
                .search("Family")
                .into_iter()
                .find(|result| result.title == "Family clip.mp4")
                .expect("queue item search hit");
            assert_eq!(
                hit.action_address(),
                Some(ActionAddress::new(
                    crate::actions::queue_item_resource_id("local-queue-local-media-a1b2-open-0"),
                    "play",
                ))
            );
            assert!(!format!("{hit:?}").contains("/home/"));
        }
        // Case `late_metadata_match_remains_cursor_paged`.
        {
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
                    media_ref: Some("fixture-media://large".into()),
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
                library: crate::actions::LocalLibraryState::default(),
            };
            let provider =
                VideoPlayerSearchProvider::new(&context, &crate::shared::SharedLibrary::default());
            let first = provider.search_page(&ui::search::SearchQuery::new("Ωmega"), None, 64, 4);
            assert!(first.inspected <= 64);
            let mut page = first;
            while page.next.is_some() && page.results.is_empty() {
                page = provider.search_page(
                    &ui::search::SearchQuery::new("Ωmega"),
                    page.next.as_ref(),
                    64,
                    4,
                );
                assert!(page.inspected <= 64, "each page obeys its scan budget");
            }
            assert_eq!(
                page.results.len(),
                1,
                "late Unicode match is eventually found"
            );
        }
    }

    #[test]
    fn shared_entity_search_resolves_to_canonical_show_and_jump_actions() {
        // Case `shared_playlists_and_annotations_search_to_canonical_show_actions`.
        {
            let context = PlayerResourceCtx {
                title: "No media",
                media: MediaAvailability {
                    has_media: false,
                    source_id: None,
                    media_ref: None,
                    can_control_transport: true,
                },
                settings: PlaybackSettingsState {
                    autoplay: true,
                    loop_enabled: false,
                    rate: 1.0,
                },
                audio_tracks: &[],
                subtitle_tracks: &[],
                chapters: &[],
                library: crate::actions::LocalLibraryState::default(),
            };
            let shared = crate::shared::SharedLibrary {
                playlists: vec![crate::shared::Playlist {
                    id: "team".into(),
                    name: "Team review".into(),
                }],
                items: Vec::new(),
                annotations: vec![crate::shared::TimestampAnnotation {
                    id: "cut".into(),
                    media_ref: "media".into(),
                    time_ms: 12_500,
                    text: "Check this cut".into(),
                }],
            };
            let provider = VideoPlayerSearchProvider::new(&context, &shared);
            assert!(
                provider
                    .search("Team review")
                    .iter()
                    .any(|hit| hit.action_address()
                        == Some(ActionAddress::new(
                            crate::actions::playlist_resource_id("team"),
                            "show"
                        )))
            );
            assert!(
                provider
                    .search("Check this cut")
                    .iter()
                    .any(|hit| hit.action_address()
                        == Some(ActionAddress::new(
                            crate::actions::annotation_resource_id("cut"),
                            "show"
                        )))
            );
        }
        // Case `matching_current_media_prefers_the_marker_jump_action`.
        {
            let context = PlayerResourceCtx {
                title: "Review.mp4",
                media: MediaAvailability {
                    has_media: true,
                    source_id: Some("fixture://review-open-2".into()),
                    media_ref: Some("fixture-media://review".into()),
                    can_control_transport: true,
                },
                settings: PlaybackSettingsState {
                    autoplay: true,
                    loop_enabled: false,
                    rate: 1.0,
                },
                audio_tracks: &[],
                subtitle_tracks: &[],
                chapters: &[],
                library: crate::actions::LocalLibraryState::default(),
            };
            let shared = crate::shared::SharedLibrary {
                playlists: Vec::new(),
                items: Vec::new(),
                annotations: vec![crate::shared::TimestampAnnotation {
                    id: "review-cut".into(),
                    media_ref: "fixture-media://review".into(),
                    time_ms: 12_500,
                    text: "Check this cut".into(),
                }],
            };
            let provider = VideoPlayerSearchProvider::new(&context, &shared);
            assert!(provider.search("Check this cut").iter().any(|hit| {
                hit.action_address()
                    == Some(ActionAddress::new(
                        crate::actions::annotation_resource_id("review-cut"),
                        "jump",
                    ))
            }));
        }
    }
}
