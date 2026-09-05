//! The open project as a qOS global-search source.
//!
//! CLAUDE.md names search a system service: an app registers a provider and a
//! renderer per resource type, so a hit opens the app *at that position*. The
//! editor had neither, which meant the one thing a person most often wants to
//! find — "where was that shot of the coastline?", "where did I put the
//! marker about the cut?" — was reachable only by scrolling the timeline.
//!
//! Four resource kinds are indexed, chosen because each is something a person
//! names and then has to find again:
//!
//!   * **Markers** — project and per-clip. These exist precisely to be
//!     navigated back to, so they are the most valuable of the three, and
//!     their notes are searched as well as their names.
//!   * **Clips** — by their source's name and exact timeline placement.
//!   * **Tracks** — by the lane name.
//!   * **Media** — by the name it carries in the bin; title words are searched
//!     as part of their media resource.
//!
//! The query path touches no decoder and no disk — it reads the in-memory
//! `Project`, which `sync` already keeps current.

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
    fuzzy_score,
};

use crate::model::Project;
use crate::timecode;

/// Result-group headers. Separate kinds so a mixed query reads as sections
/// rather than one flat list of unrelated things.
pub const MARKER_KIND: &str = "Timeline markers";
pub const CLIP_KIND: &str = "Timeline clips";
pub const TRACK_KIND: &str = "Tracks";
pub const MEDIA_KIND: &str = "Media";
const MAX_SEARCH_TEXT_CHARS: usize = 65_536;

fn capped_text(text: &str) -> String {
    text.chars().take(MAX_SEARCH_TEXT_CHARS).collect()
}

/// A [`SearchProvider`] over a snapshot of the open project.
pub struct ProjectSearchProvider {
    rows: Vec<ProjectSearchRow>,
}

struct ProjectSearchRow {
    kind: &'static str,
    title: String,
    subtitle: String,
    address: ActionAddress,
    icon: Icon,
    secondary: Option<(String, i32)>,
}

impl ProjectSearchProvider {
    /// Snapshot the current project into a provider.
    pub fn new(project: &Project) -> Self {
        use std::collections::HashMap;

        let mut rows = Vec::new();
        let mut markers = project
            .markers
            .iter()
            .map(|marker| (marker.t, marker))
            .chain(
                project
                    .clips
                    .iter()
                    .take(DEFAULT_SEARCH_INDEX_LIMIT)
                    .flat_map(|clip| {
                        clip.markers
                            .iter()
                            .map(move |marker| (clip.start + marker.t, marker))
                    }),
            )
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .collect::<Vec<_>>();
        markers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (at, marker) in markers {
            rows.push(ProjectSearchRow {
                kind: MARKER_KIND,
                title: marker.name.clone(),
                subtitle: if marker.note.trim().is_empty() {
                    timecode::clock(at)
                } else {
                    format!("{} · {}", timecode::clock(at), capped_text(&marker.note))
                },
                address: crate::actions::reveal_marker_address(marker.id),
                icon: Icon::Star,
                secondary: (!marker.note.trim().is_empty())
                    .then(|| (capped_text(&marker.note), -20)),
            });
        }

        for track in project
            .tracks
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT.saturating_sub(rows.len()))
        {
            rows.push(ProjectSearchRow {
                kind: TRACK_KIND,
                title: track.name.clone(),
                subtitle: format!("{} track", track.kind.as_str()),
                address: crate::actions::reveal_track_address(track.id),
                icon: Icon::Layers,
                secondary: None,
            });
        }
        let media = project
            .media
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .map(|item| (item.id, item))
            .collect::<HashMap<_, _>>();
        let track_names = project
            .tracks
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .map(|track| (track.id, track.name.as_str()))
            .collect::<HashMap<_, _>>();
        for clip in project
            .clips
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT.saturating_sub(rows.len()))
        {
            let Some(item) = media.get(&clip.media) else {
                continue;
            };
            let track = track_names
                .get(&clip.track)
                .copied()
                .unwrap_or("Unknown track");
            rows.push(ProjectSearchRow {
                kind: CLIP_KIND,
                title: item.name.clone(),
                subtitle: format!("{track} · {}", timecode::clock(clip.start)),
                address: crate::actions::reveal_clip_address(clip.id),
                icon: Icon::Film,
                secondary: None,
            });
        }
        let mut uses = HashMap::new();
        for clip in project.clips.iter().take(DEFAULT_SEARCH_INDEX_LIMIT) {
            *uses.entry(clip.media).or_insert(0usize) += 1;
        }
        for item in project
            .media
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT.saturating_sub(rows.len()))
        {
            let count = uses.get(&item.id).copied().unwrap_or(0);
            rows.push(ProjectSearchRow {
                kind: MEDIA_KIND,
                title: item.name.clone(),
                subtitle: if count == 0 {
                    format!("{} · in the bin", item.kind.as_str())
                } else {
                    format!(
                        "{} · used {count} time{}",
                        item.kind.as_str(),
                        if count == 1 { "" } else { "s" }
                    )
                },
                address: crate::actions::reveal_media_address(item.id),
                icon: if item.text.is_some() {
                    Icon::FileText
                } else {
                    Icon::Film
                },
                secondary: item
                    .text
                    .as_ref()
                    .map(|text| (capped_text(&text.content), 0)),
            });
        }
        Self { rows }
    }
}

impl SearchProvider for ProjectSearchProvider {
    fn name(&self) -> &str {
        MARKER_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.rows
            .iter()
            .filter_map(|row| {
                let score = fuzzy_score(query, &row.title)
                    .into_iter()
                    .chain(row.secondary.as_ref().and_then(|(text, penalty)| {
                        fuzzy_score(query, text).map(|score| score + penalty)
                    }))
                    .max()?;
                Some(
                    SearchResult::for_action(row.kind, &row.title, row.address.clone())
                        .with_subtitle(&row.subtitle)
                        .with_icon(row.icon)
                        .with_score(score),
                )
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind, Project, TextSource, Track, TrackKind};

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
        let track = uuid::Uuid::new_v4();
        p.tracks.push(Track {
            id: track,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            ..Default::default()
        });
        let clip = p.add_clip(track, media, 12.0).expect("place the clip");
        p.clip_mut(clip).expect("clip").out_point = 30.0;
        p
    }

    /// Searching separates the source from its exact placed clip and gives
    /// both a canonical address.
    #[test]
    fn a_shot_is_findable_as_media_and_as_an_exact_clip() {
        let p = project_with_things();
        let provider = ProjectSearchProvider::new(&p);

        let hits = provider.search("coastline");
        let media = hits
            .iter()
            .find(|r| r.kind == MEDIA_KIND)
            .expect("the shot is findable by a word from its name");
        assert_eq!(
            media.action_address(),
            Some(crate::actions::reveal_media_address(p.media[0].id))
        );
        let clip = hits
            .iter()
            .find(|r| r.kind == CLIP_KIND)
            .expect("the placed occurrence is independently findable");
        assert_eq!(
            clip.action_address(),
            Some(crate::actions::reveal_clip_address(p.clips[0].id))
        );
        assert!(
            clip.subtitle
                .as_deref()
                .unwrap_or("")
                .contains(&timecode::clock(12.0))
        );
    }

    /// A marker is findable by its **note**, not only its name — which is
    /// what a person actually remembers about it.
    #[test]
    fn a_marker_is_findable_by_the_note_written_on_it() {
        let mut p = project_with_things();
        p.markers.push(crate::model::Marker {
            id: uuid::Uuid::new_v4(),
            t: 21.5,
            name: "m2".into(),
            palette: 0,
            note: "hold on the wide before the cut".into(),
        });
        let provider = ProjectSearchProvider::new(&p);

        let hits = provider.search("wide before");
        let hit = hits
            .iter()
            .find(|r| r.kind == MARKER_KIND)
            .expect("a marker is findable by its note");
        assert_eq!(hit.title, "m2", "the row still reads as the marker's name");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::reveal_marker_address(p.markers[0].id))
        );
    }

    /// Media sitting in the bin is a real resource even before it is placed.
    #[test]
    fn unused_media_is_still_findable_in_the_bin() {
        let mut p = project_with_things();
        p.media.push(MediaItem {
            id: uuid::Uuid::new_v4(),
            name: "Unused_Coastline.mov".into(),
            kind: MediaKind::Video,
            duration: 5.0,
            ..Default::default()
        });
        let provider = ProjectSearchProvider::new(&p);

        let names: Vec<String> = provider
            .search("coastline")
            .iter()
            .map(|r| r.title.clone())
            .collect();
        assert!(
            names.iter().any(|n| n == "Drone_Coastline.mov"),
            "the placed shot is offered: {names:?}",
        );
        assert!(
            names.iter().any(|n| n == "Unused_Coastline.mov"),
            "{names:?}"
        );
    }

    /// A title is findable by the words it displays.
    #[test]
    fn a_title_is_findable_by_its_words() {
        let mut p = project_with_things();
        let tid = uuid::Uuid::new_v4();
        p.media.push(MediaItem {
            id: tid,
            name: "Title 1".into(),
            kind: MediaKind::Text,
            duration: 4.0,
            text: Some(TextSource {
                content: "Chapter One\nThe coast road".into(),
                ..Default::default()
            }),
            ..Default::default()
        });
        let track = p.tracks[0].id;
        let clip = p.add_clip(track, tid, 40.0).expect("place the title");
        p.clip_mut(clip).expect("clip").out_point = 4.0;

        let provider = ProjectSearchProvider::new(&p);
        let hits = provider.search("chapter");
        let hit = hits
            .iter()
            .find(|r| {
                r.kind == MEDIA_KIND
                    && r.action_address() == Some(crate::actions::reveal_media_address(tid))
            })
            .expect("a title is findable by its own words");
        assert_eq!(hit.title, "Title 1");
    }

    /// Every result uses broker-resolvable canonical activation.
    #[test]
    fn every_hit_has_a_canonical_action_address() {
        let mut project = project_with_things();
        project.add_marker(3.0, "Beat", 1);
        project.clips[0]
            .markers
            .push(crate::model::Marker::new(2.0, "Clip beat", 2));
        let provider = ProjectSearchProvider::new(&project);
        let hits = provider.search("");
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.action_address().is_some()));

        let resources = crate::actions::provide_resources(&crate::actions::VideoResourceCtx {
            project,
            playhead: 0.0,
            ..Default::default()
        });
        for hit in hits {
            let address = hit.action_address().expect("canonical address");
            let spec = qos_actions::door::resolve(&resources, &address)
                .unwrap_or_else(|| panic!("{} resolves through resources", hit.title));
            assert_eq!(spec.kind, qos_actions::OperationKind::View);
            assert_eq!(spec.contract.retry, qos_actions::RetryPolicy::Idempotent);
            assert_eq!(
                spec.contract.permission,
                qos_actions::PermissionRequirement::Read
            );
            assert!(!spec.contract.reversible);
            let encoded =
                qos_actions::door::encode(&resources, &address, None).unwrap_or_else(|error| {
                    panic!("{} resolves through resources: {error}", hit.title)
                });
            let operation = crate::actions::decode_operation(&encoded.type_id, &encoded.payload)
                .expect("operation decodes")
                .expect("video operation");
            assert!(matches!(
                operation,
                crate::actions::VideoEditorOperation::RevealTrack { .. }
                    | crate::actions::VideoEditorOperation::RevealClip { .. }
                    | crate::actions::VideoEditorOperation::RevealMarker { .. }
                    | crate::actions::VideoEditorOperation::RevealMedia { .. }
            ));
        }
    }

    #[test]
    fn huge_project_builds_a_bounded_projection_and_honours_hit_budget() {
        let mut project = Project::default();
        project.tracks = (0..(DEFAULT_SEARCH_INDEX_LIMIT + 500))
            .map(|index| crate::model::Track {
                id: uuid::Uuid::new_v4(),
                name: format!("Track {index}"),
                kind: crate::model::TrackKind::Video,
                index: index as u32,
                muted: false,
                locked: false,
                solo: false,
                gain_db: 0.0,
                pan: 0.0,
            })
            .collect();
        let provider = ProjectSearchProvider::new(&project);
        assert_eq!(provider.rows.len(), DEFAULT_SEARCH_INDEX_LIMIT);
        assert_eq!(provider.search_limited("Track", 11).len(), 11);
        assert!(provider.search_limited("Track", 0).is_empty());
    }
}
