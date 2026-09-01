//! The open project as a qOS global-search source.
//!
//! CLAUDE.md names search a system service: an app registers a provider and a
//! renderer per resource type, so a hit opens the app *at that position*. The
//! editor had neither, which meant the one thing a person most often wants to
//! find — "where was that shot of the coastline?", "where did I put the
//! marker about the cut?" — was reachable only by scrolling the timeline.
//!
//! Three kinds are indexed, chosen because each is something a person names
//! and then has to find again:
//!
//!   * **Markers** — project and per-clip. These exist precisely to be
//!     navigated back to, so they are the most valuable of the three, and
//!     their notes are searched as well as their names.
//!   * **Titles** — their text is the one part of a timeline that is
//!     literally words, so it is what someone will type.
//!   * **Media** — by the name it carries in the bin.
//!
//! Clips are *not* indexed separately from their media: a clip's searchable
//! text is its source's name, so indexing both would return the same words
//! twice under two headings. A hit on media carries the timeline position of
//! its first clip, which is what makes it a place rather than a list entry.
//!
//! The query path touches no decoder and no disk — it reads the in-memory
//! `Project`, which `sync` already keeps current.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult, fuzzy_score};

use crate::model::Project;
use crate::timecode;

/// Result-group headers. Separate kinds so a mixed query reads as sections
/// rather than one flat list of unrelated things.
pub const MARKER_KIND: &str = "Timeline markers";
pub const TITLE_KIND: &str = "Titles";
pub const MEDIA_KIND: &str = "Media";

/// `action_id` prefix for a row that seeks to an instant.
///
/// The payload is the timeline position in milliseconds rather than an id,
/// because that is what every one of these rows resolves to — a place to put
/// the playhead. An id would need a second lookup that could fail after the
/// thing was deleted, and the answer to "seek to a marker that is gone" is
/// not "do nothing", it is "do not offer the row".
const SEEK_PREFIX: &str = "video-editor.seek:";

/// Turn a chosen row's `action_id` back into the instant to seek to.
///
/// `None` for anything that is not one of ours, so a host that hands us
/// somebody else's row is refused rather than seeking to zero.
pub fn parse_action_id(action_id: &str) -> Option<f64> {
    let ms: u64 = action_id.strip_prefix(SEEK_PREFIX)?.parse().ok()?;
    // Milliseconds, so a whole-number seek is exact rather than accumulating
    // the error a float-seconds round trip would.
    Some(ms as f64 / 1000.0)
}

fn seek_id(at: f64) -> String {
    format!("{SEEK_PREFIX}{}", (at.max(0.0) * 1000.0).round() as u64)
}

/// A [`SearchProvider`] over a snapshot of the open project.
pub struct ProjectSearchProvider {
    project: Project,
}

impl ProjectSearchProvider {
    /// Snapshot the current project into a provider.
    pub fn new(project: &Project) -> Self {
        Self {
            project: project.clone(),
        }
    }

    /// Where a media item first appears on the timeline, if it does.
    fn first_use(&self, media: crate::model::MediaId) -> Option<f64> {
        self.project
            .clips
            .iter()
            .filter(|c| c.media == media)
            .map(|c| c.start)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }
}

impl SearchProvider for ProjectSearchProvider {
    fn name(&self) -> &str {
        MARKER_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let mut out = Vec::new();

        for (at, marker, _owner) in self.project.all_markers() {
            // The note is searched as well as the name: a marker often says
            // "hold on the wide before the cut" and that is what the person
            // will remember, not whatever short name they typed.
            let name_score = fuzzy_score(query, &marker.name);
            let note_score = (!marker.note.trim().is_empty())
                .then(|| fuzzy_score(query, &marker.note))
                .flatten()
                // A note match is a weaker signal than a name match: the name
                // is what the row reads as.
                .map(|s| s - 20);
            let Some(score) = name_score.into_iter().chain(note_score).max() else {
                continue;
            };
            out.push(SearchResult {
                title: marker.name.clone(),
                subtitle: Some(if marker.note.trim().is_empty() {
                    timecode::clock(at)
                } else {
                    format!("{} · {}", timecode::clock(at), marker.note)
                }),
                kind: MARKER_KIND.to_string(),
                action_id: seek_id(at),
                icon: Some(Icon::Star),
                score,
            });
        }

        for m in &self.project.media {
            if let Some(text) = m.text.as_ref() {
                // A title: its words are what someone will type.
                if let Some(score) = fuzzy_score(query, &text.content) {
                    let at = self.first_use(m.id);
                    out.push(SearchResult {
                        title: text.content.lines().next().unwrap_or("").to_string(),
                        subtitle: Some(match at {
                            Some(at) => format!("Title · {}", timecode::clock(at)),
                            None => "Title · not on the timeline".to_string(),
                        }),
                        kind: TITLE_KIND.to_string(),
                        action_id: at.map_or_else(String::new, seek_id),
                        icon: Some(Icon::FileText),
                        score,
                    });
                }
                continue;
            }
            let Some(score) = fuzzy_score(query, &m.name) else {
                continue;
            };
            let at = self.first_use(m.id);
            out.push(SearchResult {
                title: m.name.clone(),
                subtitle: Some(match at {
                    Some(at) => format!("{} · {}", m.kind.as_str(), timecode::clock(at)),
                    // Honest: it is in the bin but nowhere on the timeline, so
                    // there is no position to seek to.
                    None => format!("{} · in the bin, not on the timeline", m.kind.as_str()),
                }),
                kind: MEDIA_KIND.to_string(),
                action_id: at.map_or_else(String::new, seek_id),
                icon: Some(Icon::Film),
                score,
            });
        }

        out.retain(|r| !r.action_id.is_empty());
        // A row that resolves to nowhere is not offered: activating it could
        // only do nothing, and a search result that does nothing when chosen
        // is worse than one that is absent.
        out
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

    /// Searching finds a shot by name and lands the playhead where it is.
    #[test]
    fn a_shot_is_findable_by_name_and_seeks_to_where_it_sits() {
        let p = project_with_things();
        let provider = ProjectSearchProvider::new(&p);

        let hits = provider.search("coastline");
        let hit = hits
            .iter()
            .find(|r| r.kind == MEDIA_KIND)
            .expect("the shot is findable by a word from its name");
        assert_eq!(hit.title, "Drone_Coastline.mov");

        let at = parse_action_id(&hit.action_id).expect("the row names an instant");
        assert!(
            (at - 12.0).abs() < 0.01,
            "the row seeks to where the clip is, not to zero: {at}",
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
        let at = parse_action_id(&hit.action_id).expect("the row names an instant");
        assert!((at - 21.5).abs() < 0.01, "seeks to the marker: {at}");
    }

    /// Media sitting in the bin but not on the timeline is not offered,
    /// because choosing it could only do nothing.
    #[test]
    fn something_with_nowhere_to_seek_to_is_not_offered() {
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
            !names.iter().any(|n| n == "Unused_Coastline.mov"),
            "media with no position on the timeline has nowhere to seek to, \
             so offering it would give a row that does nothing: {names:?}",
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
            .find(|r| r.kind == TITLE_KIND)
            .expect("a title is findable by its own words");
        assert_eq!(
            hit.title, "Chapter One",
            "the row reads as the title's first line",
        );
        let at = parse_action_id(&hit.action_id).expect("the row names an instant");
        assert!((at - 40.0).abs() < 0.01, "seeks to the title: {at}");
    }

    /// Someone else's row is refused rather than read as a seek to zero.
    #[test]
    fn an_action_id_from_another_provider_is_refused() {
        assert_eq!(parse_action_id("chat.channel:abc"), None);
        assert_eq!(parse_action_id(""), None);
        assert_eq!(parse_action_id("video-editor.seek:notanumber"), None);
        assert_eq!(parse_action_id("video-editor.seek:1500"), Some(1.5));
    }
}
