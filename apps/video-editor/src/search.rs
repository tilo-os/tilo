//! The open project as search entries: every marker, track, clip, effect
//! and bin source is a findable place, and choosing one reveals it.
//!
//! Timeline data is a durable namespace, not a viewport: a marker near the
//! end of a large collaborative project must be as discoverable as the first
//! one, so nothing here is truncated. Searchable text is not display-trimmed
//! either: a word late in a caption or a marker note remains findable.

use std::collections::HashMap;

use ui::tilo_resource::SearchEntry;

use crate::model::{Clip, ClipEffect, Marker, MediaItem, Project, Track};
use crate::timecode;

/// Result-group headers. Separate kinds make a mixed query readable rather
/// than a flat list of unrelated timeline things.
pub const MARKER_KIND: &str = "Timeline markers";
pub const CLIP_KIND: &str = "Timeline clips";
pub const TRACK_KIND: &str = "Tracks";
pub const MEDIA_KIND: &str = "Media";
pub const EFFECT_KIND: &str = "Clip effects";

/// Every live navigable thing in `project`, each hit revealing it.
pub fn project_entries(project: &Project) -> Vec<SearchEntry> {
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

    let mut entries = Vec::new();
    for marker in &project.markers {
        entries.push(project_marker(marker));
    }
    for track in &project.tracks {
        entries.push(track_entry(track));
    }
    for clip in &project.clips {
        // A dangling placement cannot be revealed coherently, so never
        // publish an action for it. The action tree makes the same refusal
        // after a concurrent delete.
        let (Some(item), Some(track)) = (media.get(&clip.media), tracks.get(&clip.track)) else {
            continue;
        };
        entries.push(clip_entry(clip, item, track));
        for effect in &clip.effects {
            entries.push(effect_entry(clip, effect, item, track));
        }
        for marker in &clip.markers {
            entries.push(clip_marker(clip, marker, item));
        }
    }
    for item in &project.media {
        entries.push(media_entry(
            item,
            media_uses.get(&item.id).copied().unwrap_or(0),
        ));
    }
    entries
}

/// The item resource a hit opens: `open` reveals it.
pub fn anchor(field: &str, id: uuid::Uuid) -> String {
    ui::tilo_resource::items::item_resource_id::<Project>(
        field,
        &ui::tilo_resource::Id::from_raw(id.to_string()),
    )
}

fn project_marker(marker: &Marker) -> SearchEntry {
    SearchEntry::new(marker_label(marker), marker.note.clone())
        .anchored(anchor("markers", marker.id))
        .grouped(MARKER_KIND)
        .subtitled(marker_subtitle(marker.t, marker))
        .with_icon("star")
}

fn clip_marker(clip: &Clip, marker: &Marker, media: &MediaItem) -> SearchEntry {
    let at = clip.start + marker.t;
    SearchEntry::new(marker_label(marker), marker.note.clone())
        .anchored(format!(
            "{}/markers/{}",
            anchor("clips", clip.id),
            marker.id
        ))
        .grouped(MARKER_KIND)
        .subtitled(format!("At {} in {}", timecode::clock(at), media.name))
        .with_icon("star")
}

fn track_entry(track: &Track) -> SearchEntry {
    SearchEntry::new(track.name.clone(), track.kind.as_str())
        .anchored(anchor("tracks", track.id))
        .grouped(TRACK_KIND)
        .subtitled(format!("{} track", track.kind.as_str()))
        .with_icon("layers")
}

fn clip_entry(clip: &Clip, media: &MediaItem, track: &Track) -> SearchEntry {
    SearchEntry::new(media.name.clone(), media_text(media))
        .anchored(anchor("clips", clip.id))
        .grouped(CLIP_KIND)
        .subtitled(format!(
            "In {} at {}",
            track.name,
            timecode::clock(clip.start)
        ))
        .with_icon("film")
}

fn effect_entry(clip: &Clip, effect: &ClipEffect, media: &MediaItem, track: &Track) -> SearchEntry {
    SearchEntry::new(format!("{} effect", effect.kind.label()), String::new())
        .anchored(format!(
            "{}/effects/{}",
            anchor("clips", clip.id),
            effect.id
        ))
        .grouped(EFFECT_KIND)
        .subtitled(format!(
            "In {} on {} at {:.0}% strength",
            media.name,
            track.name,
            effect.amount * 100.0
        ))
        .with_icon("sparkles")
}

fn media_entry(item: &MediaItem, uses: usize) -> SearchEntry {
    let subtitle = if uses == 0 {
        format!("{} in the bin", item.kind.as_str())
    } else {
        format!(
            "{} used {uses} time{}",
            item.kind.as_str(),
            if uses == 1 { "" } else { "s" }
        )
    };
    SearchEntry::new(item.name.clone(), media_text(item))
        .anchored(anchor("media", item.id))
        .grouped(MEDIA_KIND)
        .subtitled(subtitle)
        .with_icon(if item.text.is_some() {
            "file-text"
        } else {
            "film"
        })
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
        // The subtitle is merely presentation; the complete note is the
        // entry's text, so nothing caps discovery while rows stay compact.
        let note: String = marker.note.chars().take(160).collect();
        format!("At {}: {}", timecode::clock(at), note)
    }
}

fn media_text(item: &MediaItem) -> String {
    item.text
        .as_ref()
        .map_or_else(String::new, |text| text.content.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaKind, Project, TextSource, Track, TrackKind};
    use ui::search::SearchProvider;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

    /// The editor's search source for `project`, as the framework builds it.
    fn source(project: &Project) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<Project>(project_entries(project))
    }

    fn add_track(project: &mut Project, kind: TrackKind, name: impl Into<String>) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        let index = project.tracks.len() as u32 + 1;
        project.tracks.push(Track {
            id,
            name: name.into(),
            kind,
            index,
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
            let hits = source(&p).search("coastline");
            assert!(hits.iter().any(|hit| {
                hit.kind == MEDIA_KIND && hit.action_id == anchor("media", p.media[0].id)
            }));
            assert!(hits.iter().any(|hit| {
                hit.kind == CLIP_KIND
                    && hit.action_id == anchor("clips", p.clips[0].id)
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
            let provider = source(&project);
            assert!(
                provider
                    .search_page(&ui::search::SearchQuery::parse("DEN FL", &[]), None, 8, 8)
                    .results
                    .iter()
                    .any(|hit| hit.action_id == anchor("markers", project.markers[0].id))
            );
            assert!(
                provider
                    .search_page(&ui::search::SearchQuery::parse("東京の夜", &[]), None, 8, 8)
                    .results
                    .iter()
                    .any(|hit| hit.action_id == anchor("media", title_id))
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
                    source(&project)
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let page = provider.search_page(
                &ui::search::SearchQuery::parse(LATE_HIT_NEEDLE, &[]),
                None,
                4,
                4,
            );
            assert_eq!(page.results.len(), 1, "the one track that matches");
        }
        assert_search_pagination();
    }

    fn assert_search_pagination() {
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
            let provider = source(&project);
            let mut cursor = None;
            let mut seen = 0;
            loop {
                let page = provider.search_page(
                    &ui::search::SearchQuery::parse("common", &[]),
                    cursor.as_ref(),
                    17,
                    5,
                );
                assert!(page.inspected <= 17);
                assert!(page.results.len() <= 5);
                seen += page.results.len();
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert_eq!(seen, 257);
        }
    }

    /// What picking the hit anchored at `anchor` reveals in `project`.
    fn reveal(
        project: &Project,
        anchor: &str,
    ) -> Result<crate::actions::VideoEditorEffect, ui::tilo_resource::Refusal> {
        let (_, item) = ui::tilo_resource::items::item_of_resource::<Project>(anchor)
            .expect("every hit names an item of the project");
        crate::resource::reveal_effect(project, &item)
    }

    #[test]
    fn editor_search_hits_open_current_items_and_stale_ones_are_refused() {
        // Case `every_hit_opens_a_current_item_and_a_stale_hit_is_refused`.
        {
            let mut project = project_with_things();
            let clip = project.clips[0].id;
            let provider = source(&project);
            let hit = provider
                .search("")
                .into_iter()
                .find(|hit| hit.action_id == anchor("clips", clip))
                .expect("clip is searchable before deletion");
            for hit in provider.search("") {
                assert!(
                    reveal(&project, &hit.action_id).is_ok(),
                    "{} opens an item of the current project",
                    hit.title
                );
            }

            // The provider may outlive the edit while a palette is open. The
            // current project is the authority and refuses the stale anchor
            // rather than revealing something else.
            project.clips.clear();
            project.media.clear();
            assert!(
                reveal(&project, &hit.action_id).is_err(),
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

            let provider = source(&project);
            let address = format!("{}/effects/{second}", anchor("clips", clip_id));
            let _hit = provider
                .search("sepia effect")
                .into_iter()
                .find(|hit| hit.action_id == address)
                .expect("the exact second Sepia is searchable");
            assert_eq!(
                reveal(&project, &address),
                Ok(crate::actions::VideoEditorEffect::RevealEffect {
                    clip: clip_id.to_string(),
                    effect: second.to_string(),
                }),
                "the hit reveals that very instance",
            );

            project
                .clip_mut(clip_id)
                .expect("fixture clip")
                .remove_effect_id(second);
            assert!(
                reveal(&project, &address).is_err(),
                "the stale duplicate's anchor cannot reveal its neighbour",
            );
        }
    }
}
