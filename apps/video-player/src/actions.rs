//! What Video Player offers beyond the library's own verbs and field edits: the player's
//! effects on this device, and the live rows of what it shows.

use ui::framework::LiveRow;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

/// The player's effect `id`, while the kind is the one its vocabulary publishes under.
pub fn reference(id: &'static str) -> tilo_resource::ActionRef<VideoEffect, ()> {
    tilo_resource::ActionRef::new(id, &[], VideoEffect::kind())
}

/// The published tree stays responsive however many streams, chapters or library rows
/// there are; a caller still reaches an omitted row by its stable id.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;

/// What Video Player does on this device: work with no document delta, from its own
/// controls, the palette, an agent or a script. A `track`, `chapter` or `item` names one
/// stream of one opened source, so a delayed call never reaches a later video's.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "video-player.effect")]
pub enum VideoEffect {
    /// Play more video and audio formats: opens Settings at Updates.
    #[effect(view)]
    OpenFormats,
    /// Open video.
    #[effect(job, kind = "read")]
    #[effect(label = "video-open-video")]
    OpenVideo {
        #[param(
            "A video's path or stream URL on this device; omit to choose with the file picker"
        )]
        path: Option<String>,
    },
    /// Play this video.
    #[effect(row, job, kind = "read")]
    PlayQueued {
        #[param("The queue or recent video's item id")]
        item: String,
    },
    /// Remove from queue.
    #[effect(row, kind = "read")]
    #[effect(label = "video-remove-from-queue")]
    RemoveQueued {
        #[param("The queued video's item id")]
        item: String,
    },
    /// Clear recent videos.
    #[effect(destructive, kind = "read")]
    #[effect(label = "video-clear-recent")]
    ClearRecent,
    #[effect(view)]
    ShowLibrary {
        #[param("queue | recent | shared")]
        tab: String,
    },
    #[effect(view)]
    HideLibrary,
    #[effect(view)]
    Close,
    #[effect(view)]
    TogglePlayback,
    #[effect(view)]
    ToggleMute,
    #[effect(view)]
    Reveal,
    #[effect(view)]
    Seek {
        #[param("Non-negative playback position in seconds")]
        seconds: f64,
    },
    #[effect(view)]
    SeekBy {
        #[param("Signed offset from the current position (at most one hour)")]
        seconds: f64,
    },
    #[effect(view)]
    Restart,
    #[effect(view)]
    JumpToEnd,
    #[effect(view)]
    SetVolume {
        #[param("Volume from 0 (silent) to 1 (full)")]
        volume: f64,
    },
    #[effect(view)]
    SetAutoplay {
        #[param("Whether newly opened videos begin playing")]
        enabled: bool,
    },
    #[effect(view)]
    SetLoop {
        #[param("Whether the current video repeats after it finishes")]
        enabled: bool,
    },
    #[effect(view)]
    SetPlaybackRate {
        #[param("Playback speed from 0.25 to 4")]
        rate: f64,
    },
    #[effect(view, row)]
    SelectAudioTrack {
        #[param("The audio track row's id")]
        track: String,
    },
    #[effect(view, row)]
    SelectSubtitles {
        #[param("The subtitle track row's id")]
        track: String,
    },
    #[effect(view)]
    SubtitlesOff,
    #[effect(view, row)]
    JumpToChapter {
        #[param("The chapter row's id")]
        chapter: String,
    },
    #[effect(view, row)]
    JumpToAnnotation {
        #[param("The annotation's id")]
        annotation: String,
    },
    #[effect(view)]
    PictureInPicture,
    #[effect(view)]
    ToggleFullscreen,
}

impl VideoEffect {
    /// Whether this effect means anything only while a video is open.
    pub fn requires_media(&self) -> bool {
        matches!(
            self,
            Self::Close
                | Self::TogglePlayback
                | Self::ToggleMute
                | Self::Reveal
                | Self::Seek { .. }
                | Self::SeekBy { .. }
                | Self::Restart
                | Self::JumpToEnd
                | Self::SetVolume { .. }
                | Self::SetPlaybackRate { .. }
                | Self::SelectAudioTrack { .. }
                | Self::SelectSubtitles { .. }
                | Self::SubtitlesOff
                | Self::JumpToChapter { .. }
                | Self::JumpToAnnotation { .. }
                | Self::PictureInPicture
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackState {
    pub stream_id: String,
    pub index: u32,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChapterState {
    pub id: String,
    pub label: String,
    pub seconds: f64,
}

/// A queue or recent row. `id` is an opaque per-device token; neither it nor a label holds
/// a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMediaItemState {
    pub id: String,
    pub title: String,
    pub current: bool,
}

pub struct PlayerRows<'a> {
    pub title: Option<&'a str>,
    /// Exact identity of the open source, which names the stream and chapter rows.
    pub source_id: Option<&'a str>,
    pub audio_tracks: &'a [TrackState],
    pub subtitle_tracks: &'a [TrackState],
    pub chapters: &'a [ChapterState],
    pub queue: Vec<LocalMediaItemState>,
    pub recent: Vec<LocalMediaItemState>,
    /// Whether the open video is playing rather than paused.
    pub playing: bool,
}

/// A compact, deterministic token for a row id; the raw identity is resolved against the
/// player's state before anything acts on it.
pub fn identity_token_for_action(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        encoded.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        encoded.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    encoded
}

pub fn audio_resource_id(source_id: &str, stream_id: &str) -> String {
    format!(
        "video-player.tracks.audio.{}.{}",
        identity_token_for_action(source_id),
        identity_token_for_action(stream_id)
    )
}

pub fn subtitle_resource_id(source_id: &str, stream_id: &str) -> String {
    format!(
        "video-player.tracks.subtitle.{}.{}",
        identity_token_for_action(source_id),
        identity_token_for_action(stream_id)
    )
}

pub fn chapter_resource_id(source_id: &str, chapter_id: &str) -> String {
    format!(
        "video-player.chapters.{}.{}",
        identity_token_for_action(source_id),
        identity_token_for_action(chapter_id)
    )
}

pub fn queue_item_resource_id(item_id: &str) -> String {
    format!(
        "video-player.queue.item.{}",
        identity_token_for_action(item_id)
    )
}

pub fn recent_item_resource_id(item_id: &str) -> String {
    format!(
        "video-player.history.item.{}",
        identity_token_for_action(item_id)
    )
}

/// The live rows of what the player shows: the open video's tracks and chapters, this
/// device's queue and recent videos, each bounded to `limit` rows.
pub(crate) fn live_rows(rows: &PlayerRows<'_>, limit: usize) -> Vec<LiveRow> {
    let mut published = Vec::new();
    if let Some(title) = rows.title {
        let mut media = LiveRow::new("video-player.media.current", "media.video", title);
        if rows.source_id.is_some() {
            let mut tracks = LiveRow::new(
                "video-player.tracks.current",
                "media.tracks",
                format!(
                    "{} audio tracks and {} subtitle tracks",
                    rows.audio_tracks.len(),
                    rows.subtitle_tracks.len()
                ),
            );
            if rows.subtitle_tracks.iter().any(|track| track.selected) {
                tracks = tracks.effect(&VideoEffect::SubtitlesOff);
            }
            let tracks = audio_rows(rows)
                .take(limit)
                .chain(subtitle_rows(rows).take(limit))
                .fold(tracks, LiveRow::row);
            let chapters = chapter_rows(rows).take(limit).fold(
                LiveRow::new(
                    "video-player.chapters.current",
                    "media.chapters",
                    format!("{} chapters available", rows.chapters.len()),
                ),
                LiveRow::row,
            );
            media = media.row(tracks).row(chapters);
        }
        published.push(media);
    }
    // Showing the queue or the recents is the player's own view, not these rows': a view
    // here would be a private disclosure that keeps removing a queued video and clearing
    // the recents from agents.
    let queue = LiveRow::new(
        "video-player.queue.local",
        "media.queue",
        format!(
            "{} session queue item{}",
            rows.queue.len(),
            if rows.queue.len() == 1 { "" } else { "s" }
        ),
    );
    published.push(queue_rows(rows).take(limit).fold(queue, LiveRow::row));
    let mut recent = LiveRow::new(
        "video-player.history.local",
        "media.history",
        format!("{} recent videos on this device", rows.recent.len()),
    );
    if !rows.recent.is_empty() {
        recent = recent.effect(&VideoEffect::ClearRecent);
    }
    published.push(recent_rows(rows).take(limit).fold(recent, LiveRow::row));
    published
}

/// The live row `id`: a collection, or a row the bounded tree leaves out.
pub(crate) fn live_row(rows: &PlayerRows<'_>, id: &str) -> Option<LiveRow> {
    audio_rows(rows)
        .chain(subtitle_rows(rows))
        .chain(chapter_rows(rows))
        .chain(queue_rows(rows))
        .chain(recent_rows(rows))
        .find(|row| row.id() == id)
}

fn track_label(track: &TrackState) -> String {
    if track.selected {
        format!("{} is selected", track.label)
    } else {
        track.label.clone()
    }
}

fn audio_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.audio_tracks
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |track| {
            let id = audio_resource_id(source_id, &track.stream_id);
            LiveRow::new(id.clone(), "media.audio-track", track_label(track))
                .effect(&VideoEffect::SelectAudioTrack { track: id })
        })
}

fn subtitle_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.subtitle_tracks
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |track| {
            let id = subtitle_resource_id(source_id, &track.stream_id);
            LiveRow::new(id.clone(), "media.subtitle-track", track_label(track))
                .effect(&VideoEffect::SelectSubtitles { track: id })
        })
}

fn chapter_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.chapters
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |chapter| {
            let id = chapter_resource_id(source_id, &chapter.id);
            LiveRow::new(
                id.clone(),
                "media.chapter",
                format!("{} starts at {:.1} seconds", chapter.label, chapter.seconds),
            )
            .effect(&VideoEffect::JumpToChapter { chapter: id })
        })
}

fn queue_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    rows.queue.iter().map(|item| {
        let suffix = match (item.current, rows.playing) {
            (true, true) => " is playing",
            (true, false) => " is paused",
            (false, _) => "",
        };
        LiveRow::new(
            queue_item_resource_id(&item.id),
            "media.queue-item",
            format!("{}{suffix}", item.title),
        )
        .effect(&VideoEffect::PlayQueued {
            item: item.id.clone(),
        })
        .effect(&VideoEffect::RemoveQueued {
            item: item.id.clone(),
        })
    })
}

fn recent_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    rows.recent.iter().map(|item| {
        LiveRow::new(
            recent_item_resource_id(&item.id),
            "media.history-item",
            item.title.clone(),
        )
        .effect(&VideoEffect::PlayQueued {
            item: item.id.clone(),
        })
    })
}
