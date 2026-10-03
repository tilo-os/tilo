//! The open project as a complete, bounded Tilo global-search source.
//!
//! Timeline data is a durable namespace, not a viewport. Do not use
//! `DEFAULT_SEARCH_INDEX_LIMIT` here: a marker near the end of a large
//! collaborative project must be as discoverable as the first one. The shared
//! [`SearchCatalog`] retains the complete in-memory projection and bounds each
//! query page by work and result count instead.

use std::collections::HashMap;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use ui::icons::Icon;

use crate::model::{Clip, ClipEffect, Marker, MediaItem, Project, Track};
use crate::timecode;

/// Result-group headers. Separate kinds make a mixed query readable rather
/// than a flat list of unrelated timeline things.
pub const MARKER_KIND: &str = "Timeline markers";
pub const CLIP_KIND: &str = "Timeline clips";
pub const TRACK_KIND: &str = "Tracks";
pub const MEDIA_KIND: &str = "Media";
pub const EFFECT_KIND: &str = "Clip effects";

/// A complete, cheap-to-clone catalogue over the current document snapshot.
///
/// The project is already resident for rendering, so building this projection
/// never opens a decoder or reads a file. Search text is deliberately not
/// display-truncated: a word late in a caption or marker note remains
/// findable. `SearchCatalog` only materialises matching result rows.
#[derive(Clone)]
pub struct ProjectSearchProvider {
    catalog: SearchCatalog,
}

impl ProjectSearchProvider {
    /// Index every live navigable resource in the supplied project snapshot.
    pub fn new(project: &Project) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new("Video editor"),
        };

        let media: HashMap<_, _> = project.media.iter().map(|item| (item.id, item)).collect();
        let tracks: HashMap<_, _> = project
            .tracks
            .iter()
            .map(|track| (track.id, track))
            .collect();
        let mut media_uses = HashMap::new();
        for clip in &project.clips {
            *media_uses.entry(clip.media).or_insert(0usize) += 1;
        }

        for marker in &project.markers {
            provider.index_project_marker(marker);
        }
        for track in &project.tracks {
            provider.index_track(track);
        }
        for clip in &project.clips {
            // A dangling placement cannot be revealed coherently, so never
            // publish a canonical action for it. The action tree makes the
            // same refusal after a concurrent delete.
            let (Some(item), Some(track)) = (media.get(&clip.media), tracks.get(&clip.track))
            else {
                continue;
            };
            provider.index_clip(clip, item, track);
            for effect in &clip.effects {
                provider.index_effect(clip, effect, item, track);
            }
            for marker in &clip.markers {
                provider.index_clip_marker(clip, marker, item);
            }
        }
        for item in &project.media {
            provider.index_media(item, media_uses.get(&item.id).copied().unwrap_or(0));
        }
        provider
    }

    fn put(&self, searchable: String, result: SearchResult) {
        let Some(address) = result.action_address() else {
            return;
        };
        // `for_action` is constructed from this exact stable id below. If a
        // future edit breaks that invariant, omit the row instead of creating
        // an opaque navigation path outside the action/resource authority.
        let _ = self.catalog.upsert(address.resource, searchable, result);
    }

    fn index_project_marker(&self, marker: &Marker) {
        let title = marker_label(marker);
        let subtitle = marker_subtitle(marker.t, marker);
        self.put(
            format!("{}\0{}", title, marker.note),
            SearchResult::for_action(
                MARKER_KIND,
                title,
                crate::actions::reveal_marker_address(marker.id),
            )
            .with_subtitle(subtitle)
            .with_icon(Icon::Star),
        );
    }

    fn index_clip_marker(&self, clip: &Clip, marker: &Marker, media: &MediaItem) {
        let title = marker_label(marker);
        let at = clip.start + marker.t;
        self.put(
            format!("{}\0{}\0{}", title, marker.note, media.name),
            SearchResult::for_action(
                MARKER_KIND,
                title,
                crate::actions::reveal_marker_address(marker.id),
            )
            .with_subtitle(format!("{} · {}", timecode::clock(at), media.name))
            .with_icon(Icon::Star),
        );
    }

    fn index_track(&self, track: &Track) {
        self.put(
            format!("{}\0{}", track.name, track.kind.as_str()),
            SearchResult::for_action(
                TRACK_KIND,
                track.name.clone(),
                crate::actions::reveal_track_address(track.id),
            )
            .with_subtitle(format!("{} track", track.kind.as_str()))
            .with_icon(Icon::Layers),
        );
    }

    fn index_clip(&self, clip: &Clip, media: &MediaItem, track: &Track) {
        self.put(
            format!("{}\0{}\0{}", media.name, track.name, media_text(media)),
            SearchResult::for_action(
                CLIP_KIND,
                media.name.clone(),
                crate::actions::reveal_clip_address(clip.id),
            )
            .with_subtitle(format!("{} · {}", track.name, timecode::clock(clip.start)))
            .with_icon(Icon::Film),
        );
    }

    fn index_effect(&self, clip: &Clip, effect: &ClipEffect, media: &MediaItem, track: &Track) {
        let title = format!("{} effect", effect.kind.label());
        self.put(
            format!("{}\0{}\0{}", title, media.name, track.name),
            SearchResult::for_action(
                EFFECT_KIND,
                title,
                crate::actions::reveal_effect_address(clip.id, effect.id),
            )
            .with_subtitle(format!(
                "{} · {} · {:.0}%",
                media.name,
                track.name,
                effect.amount * 100.0
            ))
            .with_icon(Icon::Sparkles),
        );
    }

    fn index_media(&self, item: &MediaItem, uses: usize) {
        let subtitle = if uses == 0 {
            format!("{} · in the bin", item.kind.as_str())
        } else {
            format!(
                "{} · used {uses} time{}",
                item.kind.as_str(),
                if uses == 1 { "" } else { "s" }
            )
        };
        self.put(
            format!(
                "{}\0{}\0{}",
                item.name,
                item.kind.as_str(),
                media_text(item)
            ),
            SearchResult::for_action(
                MEDIA_KIND,
                item.name.clone(),
                crate::actions::reveal_media_address(item.id),
            )
            .with_subtitle(subtitle)
            .with_icon(if item.text.is_some() {
                Icon::FileText
            } else {
                Icon::Film
            }),
        );
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

fn marker_label(marker: &Marker) -> String {
    if marker.name.trim().is_empty() {
        "Marker".to_string()
    } else {
        marker.name.clone()
    }
}

fn marker_subtitle(at: f64, marker: &Marker) -> String {
    if marker.note.trim().is_empty() {
        timecode::clock(at)
    } else {
        // The subtitle is merely presentation; retaining the complete note in
        // the index above avoids a hidden discovery cap while keeping palette
        // rows compact and responsive.
        let note: String = marker.note.chars().take(160).collect();
        format!("{} · {}", timecode::clock(at), note)
    }
}

fn media_text(item: &MediaItem) -> &str {
    item.text.as_ref().map_or("", |text| text.content.as_str())
}

impl CatalogSearchProvider for ProjectSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Video editor"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaKind, Project, TextSource, Track, TrackKind};
    use ui::search::SearchProvider;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

    fn add_track(project: &mut Project, kind: TrackKind, name: impl Into<String>) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        project.tracks.push(Track {
            id,
            name: name.into(),
            kind,
            index: project.tracks.len() as u32 + 1,
            ..Default::default()
        });
        id
    }

    fn project_with_things() -> Project {
        let mut p = Project::new("search");
        let media = uuid::Uuid::new_v4();
        p.media.push(MediaItem {
            id: media,
            name: "Drone_Coastline.mov".into(),
            kind: MediaKind::Video,
            duration: 30.0,
            ..Default::default()
        });
        let track = add_track(&mut p, TrackKind::Video, "V1");
        let clip = p.add_clip(track, media, 12.0).expect("place the clip");
        p.clip_mut(clip).expect("clip").out_point = 30.0;
        p
    }

    #[test]
    fn editor_search_finds_media_clips_captions_and_late_tracks() {
        // Case `a_shot_is_findable_as_media_and_as_an_exact_clip`.
        {
            let p = project_with_things();
            let hits = ProjectSearchProvider::new(&p).search("coastline");
            assert!(hits.iter().any(|hit| {
                hit.kind == MEDIA_KIND
                    && hit.action_address()
                        == Some(crate::actions::reveal_media_address(p.media[0].id))
            }));
            assert!(hits.iter().any(|hit| {
                hit.kind == CLIP_KIND
                    && hit.action_address()
                        == Some(crate::actions::reveal_clip_address(p.clips[0].id))
                    && hit
                        .subtitle
                        .as_deref()
                        .is_some_and(|text| text.contains(&timecode::clock(12.0)))
            }));
        }
        // Case `caption_and_marker_note_search_preserve_unicode_and_late_text`.
        {
            let mut project = project_with_things();
            project.markers.push(crate::model::Marker {
                id: uuid::Uuid::new_v4(),
                t: 21.5,
                name: "m2".into(),
                palette: 0,
                note: format!("{} Über den Fluss", "ordinary ".repeat(10_000)),
            });
            let title_id = uuid::Uuid::new_v4();
            project.media.push(MediaItem {
                id: title_id,
                name: "Title 1".into(),
                kind: MediaKind::Text,
                duration: 4.0,
                text: Some(TextSource {
                    content: format!("{} 東京の夜", "caption ".repeat(10_000)),
                    ..Default::default()
                }),
                ..Default::default()
            });
            let provider = ProjectSearchProvider::new(&project);
            assert!(
                provider
                    .search_page("DEN FL", None, 8, 8)
                    .results
                    .iter()
                    .any(|hit| {
                        hit.action_address()
                            == Some(crate::actions::reveal_marker_address(project.markers[0].id))
                    })
            );
            assert!(
                provider
                    .search_page("東京の夜", None, 8, 8)
                    .results
                    .iter()
                    .any(|hit| {
                        hit.action_address() == Some(crate::actions::reveal_media_address(title_id))
                    })
            );
        }
        // Case `late_track_remains_findable_in_a_large_project`.
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let mut project = Project::new("large");
                    for row in corpus {
                        add_track(&mut project, TrackKind::Video, row.text);
                    }
                    ProjectSearchProvider::new(&project)
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let page = provider.search_page(LATE_HIT_NEEDLE, None, 4, 4);
            assert_eq!(page.inspected, 1, "rare posting avoids a corpus scan");
            assert_eq!(provider.stats().records, 3_000);
        }
        // Case `pages_bound_work_and_materialisation_without_losing_broad_hits`.
        {
            let mut project = Project::new("large");
            for number in 0..257 {
                add_track(
                    &mut project,
                    TrackKind::Video,
                    format!("Common track {number}"),
                );
            }
            let provider = ProjectSearchProvider::new(&project);
            let mut cursor = None;
            let mut seen = 0;
            loop {
                let page = provider.search_page("common", cursor.as_ref(), 17, 5);
                assert!(page.inspected <= 17);
                assert!(page.results.len() <= 5);
                seen += page.results.len();
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert_eq!(seen, 257);
        }
    }

    #[test]
    fn editor_search_hits_are_current_canonical_view_actions() {
        // Case `every_hit_is_a_current_canonical_view_action_and_stale_hit_is_refused`.
        {
            let mut project = project_with_things();
            let clip = project.clips[0].id;
            let provider = ProjectSearchProvider::new(&project);
            let hit = provider
                .search("")
                .into_iter()
                .find(|hit| hit.action_address() == Some(crate::actions::reveal_clip_address(clip)))
                .expect("clip is searchable before deletion");
            let resources = crate::actions::provide_resources(&crate::actions::VideoResourceCtx {
                project: project.clone(),
                ..Default::default()
            });
            for hit in provider.search("") {
                let address = hit.action_address().expect("search is canonical");
                assert!(
                    tilo_actions::door::resolve(&resources, &address)
                        .is_some_and(|spec| spec.kind == tilo_actions::OperationKind::View),
                    "{} resolves through the current resource tree",
                    hit.title
                );
            }

            // The provider may outlive the edit while a palette is open. The fresh
            // canonical resource tree is the authority and must reject its stale
            // address rather than letting the old row navigate by callback.
            project.clips.clear();
            project.media.clear();
            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&crate::actions::provide_resources(
                &crate::actions::VideoResourceCtx {
                    project,
                    ..Default::default()
                },
            ));
            palette.set_providers(vec![Box::new(provider)]);
            palette.open();
            palette.set_query(&hit.title);
            let row = palette
                .result_labels()
                .iter()
                .position(|label| label == &hit.title)
                .expect("stale row remains displayable");
            palette.set_cursor(row);
            palette.activate_selected();
            assert!(
                palette.take_operation().is_none(),
                "deleted target is refused"
            );
        }
        // Case `duplicate_effect_instances_are_searchable_by_id_and_a_stale_one_is_refused`.
        {
            let mut project = project_with_things();
            let clip_id = project.clips[0].id;
            let clip = project.clip_mut(clip_id).expect("fixture clip");
            clip.add_effect(crate::model::EffectKind::Sepia);
            clip.add_effect(crate::model::EffectKind::Sepia);
            let first = clip.effects[0].id;
            let second = clip.effects[1].id;
            assert_ne!(first, second, "two Sepias are distinct durable instances");

            let provider = ProjectSearchProvider::new(&project);
            let address = crate::actions::reveal_effect_address(clip_id, second);
            let _hit = provider
                .search("sepia effect")
                .into_iter()
                .find(|hit| hit.action_address() == Some(address.clone()))
                .expect("the exact second Sepia is searchable");
            let resources = crate::actions::provide_resources(&crate::actions::VideoResourceCtx {
                project: project.clone(),
                ..Default::default()
            });
            assert!(
                tilo_actions::door::resolve(&resources, &address).is_some(),
                "search address resolves through the canonical resource tree",
            );

            project
                .clip_mut(clip_id)
                .expect("fixture clip")
                .remove_effect_id(second);
            let current_resources =
                crate::actions::provide_resources(&crate::actions::VideoResourceCtx {
                    project,
                    ..Default::default()
                });
            assert!(
                tilo_actions::door::resolve(&current_resources, &address).is_none(),
                "the stale duplicate's canonical address cannot resolve to its neighbour",
            );
        }
    }
}
