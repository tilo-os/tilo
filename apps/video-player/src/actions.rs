//! What Video Player offers beyond the library's own verbs and field edits: the player's
//! effects on this device, and the live rows of what it shows.

use ui::framework::LiveRow;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::{Effect, RowBindings};

/// The player's effect `id`, while the kind is the one its vocabulary publishes under.
pub fn reference(id: &'static str) -> tilo_resource::ActionRef<VideoEffect, ()> {
    tilo_resource::ActionRef::new(id, &[], VideoEffect::kind())
}

/// The open video, as a row: its tracks and chapters hang beneath it.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.video", label = "Video")]
pub struct OpenVideo;

/// Every audio and subtitle track of the open video, as one row under it.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.tracks", label = "Tracks", effects = VideoEffect)]
pub struct Tracks;

/// The chapters of the open video, as one row under it.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.chapters", label = "Chapters")]
pub struct Chapters;

/// This device's session queue, as a row.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.queue", label = "Queue")]
pub struct LocalQueue;

/// The videos this device played, as a row. Clearing them is the row's own
/// effect; showing the library is the player's view, not theirs.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.history", label = "Recent videos", effects = VideoEffect)]
pub struct LocalHistory;

/// One track of the open video's audio.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.audio-track", label = "Audio track", effects = VideoEffect)]
pub struct AudioTrack;

/// One subtitle track of the open video.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.subtitle-track", label = "Subtitle track", effects = VideoEffect)]
pub struct SubtitleTrack;

/// One chapter of the open video.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.chapter", label = "Chapter", effects = VideoEffect)]
pub struct Chapter;

/// One video queued on this device.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.queue-item", label = "Queued video", effects = VideoEffect)]
pub struct QueueItem;

/// One video this device played.
#[derive(tilo_resource::Row)]
#[row(kind = "video-player.history-item", label = "Recent video", effects = VideoEffect)]
pub struct HistoryItem;

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
    #[effect(row, job, kind = "read", on = QueueItem, on = HistoryItem)]
    PlayQueued {
        #[param("The queue or recent video's item id")]
        item: String,
    },
    /// Remove from queue.
    #[effect(row, kind = "read", on = QueueItem)]
    #[effect(label = "video-remove-from-queue")]
    RemoveQueued {
        #[param("The queued video's item id")]
        item: String,
    },
    /// Clear recent videos.
    #[effect(destructive, kind = "read", on = LocalHistory)]
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
    #[effect(view, row, on = AudioTrack)]
    SelectAudioTrack {
        #[param("The audio track row's id")]
        track: String,
    },
    #[effect(view, row, on = SubtitleTrack)]
    SelectSubtitles {
        #[param("The subtitle track row's id")]
        track: String,
    },
    #[effect(view, on = Tracks)]
    SubtitlesOff,
    #[effect(view, row, on = Chapter)]
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
        let mut media = LiveRow::typed::<OpenVideo>("video-player.media.current", title);
        if rows.source_id.is_some() {
            // Turning subtitles off is offered while one is on.
            let mut bindings = RowBindings::new();
            if rows.subtitle_tracks.iter().any(|track| track.selected) {
                bindings = bindings.offer(VideoEffect::SUBTITLES_OFF);
            }
            let tracks = LiveRow::typed_with::<Tracks>(
                "video-player.tracks.current",
                format!(
                    "{} audio tracks and {} subtitle tracks",
                    rows.audio_tracks.len(),
                    rows.subtitle_tracks.len()
                ),
                &bindings,
            );
            let tracks = audio_rows(rows)
                .take(limit)
                .chain(subtitle_rows(rows).take(limit))
                .fold(tracks, LiveRow::row);
            let chapters = chapter_rows(rows).take(limit).fold(
                LiveRow::typed::<Chapters>(
                    "video-player.chapters.current",
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
    let queue = LiveRow::typed::<LocalQueue>(
        "video-player.queue.local",
        format!(
            "{} session queue item{}",
            rows.queue.len(),
            if rows.queue.len() == 1 { "" } else { "s" }
        ),
    );
    published.push(queue_rows(rows).take(limit).fold(queue, LiveRow::row));
    // Clearing the recents is offered while there are any.
    let mut bindings = RowBindings::new();
    if !rows.recent.is_empty() {
        bindings = bindings.offer(VideoEffect::CLEAR_RECENT);
    }
    let recent = LiveRow::typed_with::<LocalHistory>(
        "video-player.history.local",
        format!("{} recent videos on this device", rows.recent.len()),
        &bindings,
    );
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
            LiveRow::typed::<AudioTrack>(
                audio_resource_id(source_id, &track.stream_id),
                track_label(track),
            )
        })
}

fn subtitle_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.subtitle_tracks
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |track| {
            LiveRow::typed::<SubtitleTrack>(
                subtitle_resource_id(source_id, &track.stream_id),
                track_label(track),
            )
        })
}

fn chapter_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.chapters
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |chapter| {
            LiveRow::typed::<Chapter>(
                chapter_resource_id(source_id, &chapter.id),
                format!("{} starts at {:.1} seconds", chapter.label, chapter.seconds),
            )
        })
}

/// The entry `item` names, as the effects offered on its row act on it.
fn entry_bindings(id: &str) -> RowBindings {
    let item = serde_json::Value::from(id.to_owned());
    RowBindings::new()
        .bind(VideoEffect::PLAY_QUEUED, [("item", item.clone())])
        .bind(VideoEffect::REMOVE_QUEUED, [("item", item)])
}

fn queue_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    rows.queue.iter().map(|item| {
        let suffix = match (item.current, rows.playing) {
            (true, true) => " is playing",
            (true, false) => " is paused",
            (false, _) => "",
        };
        LiveRow::typed_with::<QueueItem>(
            queue_item_resource_id(&item.id),
            format!("{}{suffix}", item.title),
            &entry_bindings(&item.id),
        )
    })
}

fn recent_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    rows.recent.iter().map(|item| {
        let item_arguments = serde_json::Value::from(item.id.clone());
        let bindings =
            RowBindings::new().bind(VideoEffect::PLAY_QUEUED, [("item", item_arguments)]);
        LiveRow::typed_with::<HistoryItem>(
            recent_item_resource_id(&item.id),
            item.title.clone(),
            &bindings,
        )
    })
}
